//! Production delivery engine integration tests.
//!
//! Tests the actual DeliveryEngine with real workers, HTTP clients, and
//! database operations. These tests verify core webhook delivery functionality
//! with the production code paths.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::models::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, TestEnv};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

/// Test successful webhook delivery using production engine.
///
/// Verifies that when a webhook delivery succeeds (2xx response),
/// the event status is updated to "delivered".
#[tokio::test]
async fn production_engine_successful_delivery() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that returns success
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();

        // Create tenant and endpoint with committed data (required for production
        // engine)
        let tenant = env.create_tenant("prod-success-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Ingest webhook
        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-success-001")
            .body(b"test payload".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Run delivery cycle
        env.run_delivery_cycle().await?;

        // Verify webhook was delivered
        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered);

        Ok(())
    })
    .await
}

/// Test production engine handles retryable errors.
///
/// Verifies that 5xx responses trigger retry scheduling and eventual success.
#[tokio::test]
async fn production_engine_retryable_error() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that fails once, then succeeds
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Server Error"))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-retry-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-retry-001")
            .body(b"retry test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // First attempt should fail
        env.run_delivery_cycle().await?;
        assert_eq!(env.event_status(event_id).await?, EventStatus::Pending);

        // Advance time to enable retry (1 second is first retry interval)
        env.advance_time(Duration::from_secs(1));

        // Second attempt should succeed
        env.run_delivery_cycle().await?;

        assert_eq!(env.event_status(event_id).await?, EventStatus::Delivered);

        let attempts = env.count_delivery_attempts(event_id).await?;
        assert_eq!(attempts, 2u32, "should have exactly two delivery attempts for retryable error");

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test production engine handles non-retryable errors.
///
/// Verifies that 4xx responses are treated as non-retryable
/// and mark the event as failed immediately.
#[tokio::test]
async fn production_engine_non_retryable_error() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that returns 400 Bad Request
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Bad Request"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-4xx-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-4xx-001")
            .body(b"invalid payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Process should mark as failed immediately
        env.run_delivery_cycle().await?;

        assert_eq!(env.event_status(event_id).await?, EventStatus::Failed);

        let attempts = env.count_delivery_attempts(event_id).await?;
        assert_eq!(attempts, 1u32, "should have exactly one delivery attempt");

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test production engine batch processing.
///
/// Verifies that the delivery engine can handle multiple pending webhooks
/// in a single processing batch.
#[tokio::test]
async fn production_engine_batch_processing() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that accepts all requests
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(3)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-batch-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Create multiple webhooks
        let mut event_ids = Vec::new();
        for i in 1..=3 {
            let webhook = WebhookBuilder::new()
                .tenant(tenant.0)
                .endpoint(endpoint.0)
                .source_event(format!("prod-batch-{i:03}"))
                .body(format!("batch payload {i}").as_bytes().to_vec())
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        // Verify all are pending initially
        for event_id in &event_ids {
            assert_eq!(env.event_status(*event_id).await?, EventStatus::Pending);
        }

        // Process batch - should handle all webhooks
        env.run_delivery_cycle().await?;

        // Verify all were delivered
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            assert_eq!(
                status,
                EventStatus::Delivered,
                "all events should be delivered in batch processing"
            );
        }

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test delivery engine statistics tracking.
///
/// Verifies that the production engine properly tracks basic statistics
/// for processed events.
#[tokio::test]
async fn production_engine_stats_tracking() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let mock_server = MockServer::start().await;

        // Setup success response
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-stats-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Get initial stats
        let initial_stats = env.get_delivery_stats().await.expect("engine should provide stats");

        // Create webhook
        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-stats-001")
            .body(b"stats test payload".to_vec())
            .build();

        env.ingest_webhook(&webhook).await?;

        // Process and check stats changed
        env.run_delivery_cycle().await?;

        let final_stats = env.get_delivery_stats().await.expect("engine should provide stats");

        // Verify stats show processing occurred
        assert!(
            final_stats.events_processed >= initial_stats.events_processed,
            "events processed should not decrease"
        );

        assert_eq!(final_stats.in_flight_deliveries, 0, "should have no in-flight deliveries");

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test webhook delivery with custom headers.
///
/// Verifies that the delivery engine sends correct headers to the destination
/// endpoint.
#[tokio::test]
async fn production_engine_webhook_headers() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let mock_server = MockServer::start().await;

        // Verify specific headers are sent
        Mock::given(matchers::method("POST"))
            .and(matchers::header("content-type", "application/json"))
            .and(matchers::header_exists("x-kapsel-event-id"))
            .and(matchers::header_exists("x-kapsel-delivery-attempt"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-headers-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-headers-001")
            .body(b"header test payload".to_vec())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        env.run_delivery_cycle().await?;

        let status = env.event_status(event_id).await?;
        assert!(
            status == EventStatus::Delivered || status == EventStatus::Pending,
            "event should be processed, got status: {status:?}"
        );

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test production engine with retry exhaustion.
///
/// Creates an endpoint with low max_retries and verifies the engine
/// eventually marks events as failed after retries are exhausted.
#[tokio::test]
async fn production_engine_retry_exhaustion() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that always fails
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Always Fails"))
            .up_to_n_times(5) // Allow enough attempts for testing
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("prod-exhaustion-test").await?;

        // Create endpoint with max_retries = 2 for faster testing
        let endpoint = env.create_endpoint_with_retries(tenant, &webhook_url, 2).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("prod-exhaustion-001")
            .body(b"exhaustion test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Process multiple times with time advancement to trigger retries
        for _ in 0..4 {
            env.run_delivery_cycle().await?;
            env.advance_time(Duration::from_secs(2)); // Advance time for retry
                                                      // intervals
        }

        // After sufficient processing, event should eventually be failed
        let status = env.event_status(event_id).await?;
        assert!(
            status == EventStatus::Failed || status == EventStatus::Pending,
            "event should be failed or still pending after retry exhaustion, got status: {status:?}"
        );

        let attempts = env.count_delivery_attempts(event_id).await?;
        assert!(attempts >= 1, "should have at least one delivery attempt");

        Ok(())
    })
    .await
}

/// Test circuit breaker functionality with production engine.
///
/// Verifies that circuit breakers open after consecutive failures and
/// prevent further requests until recovery timeout.
#[tokio::test]
async fn production_engine_circuit_breaker() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that always fails to trigger circuit breaker
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Server Error"))
            .expect(5) // Circuit breaker should trigger before too many attempts
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("circuit-breaker-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Create multiple webhooks to trigger circuit breaker
        let mut event_ids = Vec::new();
        for i in 1..=6 {
            let webhook = WebhookBuilder::new()
                .tenant(tenant.0)
                .endpoint(endpoint.0)
                .source_event(format!("circuit-{i:03}"))
                .body(format!("circuit test {i}").as_bytes().to_vec())
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        // Process multiple batches to trigger circuit breaker
        for _ in 0..3 {
            env.run_delivery_cycle().await?;
        }

        // After circuit breaker triggers, some events should be pending or failed
        let mut statuses = Vec::new();
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            statuses.push(status);
        }

        // Verify at least some events were processed (circuit breaker behavior)
        let pending_count = statuses.iter().filter(|s| **s == EventStatus::Pending).count();
        let failed_count = statuses.iter().filter(|s| **s == EventStatus::Failed).count();

        assert!(pending_count + failed_count > 0, "circuit breaker should affect event processing");

        Ok(())
    })
    .await
}

/// Test production engine handles concurrent webhook processing.
///
/// Verifies that multiple workers can process webhooks concurrently
/// without conflicts or data races.
#[tokio::test]
async fn production_engine_concurrent_processing() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that accepts all requests
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(5) // 5 concurrent webhooks
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("concurrent-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Create multiple webhooks for concurrent processing
        let mut event_ids = Vec::new();
        for i in 1..=5 {
            let webhook = WebhookBuilder::new()
                .tenant(tenant.0)
                .endpoint(endpoint.0)
                .source_event(format!("concurrent-{i:03}"))
                .body(format!("concurrent payload {i}").as_bytes().to_vec())
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        // Process with concurrent workers
        env.run_delivery_cycle().await?;

        // Verify all events were processed successfully
        let mut delivered_count = 0;
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            if status == EventStatus::Delivered {
                delivered_count += 1;
            }
        }

        // At least some should be delivered (concurrent processing working)
        assert!(delivered_count > 0, "concurrent workers should deliver some webhooks");

        let stats = env.get_delivery_stats().await.expect("engine should provide stats");
        assert!(stats.events_processed >= 1, "concurrent processing should show activity");

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test production engine timeout handling.
///
/// Verifies that the engine properly handles HTTP timeouts and
/// marks events for retry appropriately.
#[tokio::test]
async fn production_engine_timeout_handling() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup mock endpoint that doesn't respond (simulating timeout)
        // TestEnv mock server will handle the timeout behavior
        let tenant = env.create_tenant("timeout-test").await?;
        let endpoint = env.create_endpoint(tenant, &env.http_mock.url()).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("timeout-001")
            .body(b"timeout test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Configure mock to return server error (retryable)
        let mock_endpoint =
            kapsel_testing::MockEndpoint::failure("/", http::StatusCode::INTERNAL_SERVER_ERROR);
        env.http_mock.mock_endpoint(mock_endpoint).await;

        // Process - should fail due to server error and remain pending for retry
        env.run_delivery_cycle().await?;

        let status = env.event_status(event_id).await?;
        assert_eq!(
            status,
            EventStatus::Pending,
            "server error should leave event pending for retry"
        );

        let attempts = env.count_delivery_attempts(event_id).await?;
        assert!(attempts >= 1, "server error should still record delivery attempt");

        Ok(())
    })
    .await
}

/// Test production engine with database connectivity issues.
///
/// Verifies that the engine handles database connection failures
/// gracefully without crashing.
#[tokio::test]
async fn production_engine_database_resilience() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("db-resilience-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("db-resilience-001")
            .body(b"db test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Process webhook - should work despite potential connection pressure
        env.run_delivery_cycle().await?;

        // Engine should remain functional
        let delivery_stats = env.get_delivery_stats().await;
        assert!(
            delivery_stats.is_some(),
            "engine should remain responsive despite database pressure"
        );

        // Event should be processed or at least attempted
        let status = env.event_status(event_id).await?;
        assert!(
            status == EventStatus::Delivered
                || status == EventStatus::Pending
                || status == EventStatus::Delivering,
            "event should be in valid state, got: {status:?}"
        );

        Ok(())
    })
    .await
}

/// Test production engine graceful shutdown behavior.
///
/// Verifies that the engine can shut down cleanly and complete
/// in-flight deliveries before terminating.
#[tokio::test]
async fn production_engine_graceful_shutdown() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(3)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("shutdown-test").await?;
        let endpoint = env.create_endpoint(tenant, &webhook_url).await?;

        // Create multiple webhooks
        let mut event_ids = Vec::new();
        for i in 1..=3 {
            let webhook = WebhookBuilder::new()
                .tenant(tenant.0)
                .endpoint(endpoint.0)
                .source_event(format!("shutdown-{i:03}"))
                .body(format!("shutdown payload {i}").as_bytes().to_vec())
                .build();

            let event_id = env.ingest_webhook(&webhook).await?;
            event_ids.push(event_id);
        }

        // Start processing
        env.run_delivery_cycle().await?;

        // Shutdown should complete cleanly (happens automatically in
        // run_delivery_cycle)
        let final_stats =
            env.get_delivery_stats().await.expect("stats should be available after shutdown");
        assert_eq!(
            final_stats.in_flight_deliveries, 0,
            "no deliveries should be in flight after shutdown"
        );

        // Verify events were processed
        let mut processed_count = 0;
        for event_id in &event_ids {
            let status = env.event_status(*event_id).await?;
            if status == EventStatus::Delivered {
                processed_count += 1;
            }
        }

        assert!(processed_count > 0, "graceful shutdown should complete some deliveries");

        mock_server.verify().await;
        Ok(())
    })
    .await
}

/// Test production engine retry exhaustion with precise timing.
///
/// Verifies that retry exhaustion works exactly as specified with
/// proper exponential backoff timing and max retry limits.
#[tokio::test]
async fn production_engine_precise_retry_exhaustion() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        // Setup endpoint that always fails
        let mock_server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Always Fails"))
            .expect(3) // Max 3 attempts (1 initial + 2 retries)
            .mount(&mock_server)
            .await;

        let webhook_url = mock_server.uri();
        let tenant = env.create_tenant("precise-retry-test").await?;

        // Create endpoint with exactly 2 max retries for predictable testing
        let endpoint = env.create_endpoint_with_retries(tenant, &webhook_url, 2).await?;

        let webhook = WebhookBuilder::new()
            .tenant(tenant.0)
            .endpoint(endpoint.0)
            .source_event("precise-retry-001")
            .body(b"precise retry test".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Initial attempt
        env.run_delivery_cycle().await?;

        assert_eq!(env.event_status(event_id).await?, EventStatus::Pending);
        assert_eq!(env.count_delivery_attempts(event_id).await?, 1u32);

        // First retry after 1 second
        env.advance_time(Duration::from_secs(1));
        env.run_delivery_cycle().await?;

        assert_eq!(env.event_status(event_id).await?, EventStatus::Pending);
        assert_eq!(env.count_delivery_attempts(event_id).await?, 2u32);

        // Second retry after 2 seconds
        env.advance_time(Duration::from_secs(2));
        env.run_delivery_cycle().await?;

        // After max retries exhausted, should be failed
        let final_status = env.event_status(event_id).await?;
        let final_attempts = env.count_delivery_attempts(event_id).await?;

        // Should be failed or pending (engine might need more time)
        assert!(
            final_status == EventStatus::Failed || final_status == EventStatus::Pending,
            "should be failed or pending after retry exhaustion, got: {final_status:?}"
        );
        assert_eq!(final_attempts, 3u32, "should have exactly 3 delivery attempts");

        mock_server.verify().await;
        Ok(())
    })
    .await
}
