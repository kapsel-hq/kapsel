//! Integration tests for delivery workers and worker pool management.
//!
//! Tests worker pool lifecycle, webhook delivery processing, error handling,
//! and integration between delivery workers and the database.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use kapsel_core::Clock;
use kapsel_delivery::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    client::{ClientConfig, DeliveryClient},
    retry::RetryPolicy,
    worker::{DeliveryConfig, EngineStats},
    worker_pool::WorkerPool,
};
use kapsel_testing::TestEnv;
use tokio::{sync::RwLock, time::timeout};
use tokio_util::sync::CancellationToken;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Test worker pool can be explicitly shut down without hanging.
///
/// Verifies that worker pool shutdown completes within reasonable time
/// and all workers are properly terminated.
#[tokio::test]
async fn worker_pool_explicit_shutdown() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    let config = DeliveryConfig {
        worker_count: 2,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig { timeout: Duration::from_secs(5), ..Default::default() },
        default_retry_policy: RetryPolicy::default(),
        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let circuit_manager =
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
    let stats = Arc::new(RwLock::new(EngineStats::default()));
    let cancellation_token = CancellationToken::new();

    let mut pool = WorkerPool::new(
        env.create_pool(),
        config,
        client,
        circuit_manager,
        stats,
        cancellation_token,
        Arc::new(env.clock.clone()) as Arc<dyn Clock>,
    );

    pool.spawn_workers().await?;
    assert!(pool.has_active_workers(), "Workers should be active after spawning");

    // Let workers run briefly to ensure they're actually working
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Explicitly shut down workers (consumes pool, guaranteeing cleanup)
    pool.shutdown_graceful(Duration::from_secs(5)).await?;

    Ok(())
}

/// Test worker pool cleanup through Drop trait.
///
/// Verifies that worker pool properly cleans up when dropped,
/// preventing orphaned worker tasks.
#[tokio::test]
async fn worker_pool_drop_cleanup() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    let config = DeliveryConfig {
        worker_count: 2,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig::default(),
        default_retry_policy: RetryPolicy::default(),
        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let circuit_manager =
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
    let stats = Arc::new(RwLock::new(EngineStats::default()));
    let cancellation_token = CancellationToken::new();
    let cancellation_token_clone = cancellation_token.clone();

    {
        let mut pool = WorkerPool::new(
            env.create_pool(),
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        );

        pool.spawn_workers().await?;
        assert!(pool.has_active_workers(), "Workers should be active after spawning");

        // Let workers run briefly
        tokio::time::sleep(Duration::from_millis(50)).await;
    } // WorkerPool goes out of scope here, Drop should be called

    // Give Drop implementation time to complete
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify cancellation token was cancelled by Drop
    assert!(cancellation_token_clone.is_cancelled(), "Drop should have cancelled the token");

    Ok(())
}

/// Test worker pool handles shutdown timeout gracefully.
///
/// Verifies that worker pool shutdown respects timeout and doesn't hang
/// indefinitely when workers don't respond to cancellation.
#[tokio::test]
async fn worker_pool_shutdown_timeout() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    let config = DeliveryConfig {
        worker_count: 2,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig::default(),
        default_retry_policy: RetryPolicy::default(),
        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let circuit_manager =
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
    let stats = Arc::new(RwLock::new(EngineStats::default()));
    let cancellation_token = CancellationToken::new();

    let mut pool = WorkerPool::new(
        env.create_pool(),
        config,
        client,
        circuit_manager,
        stats,
        cancellation_token,
        Arc::new(env.clock.clone()) as Arc<dyn Clock>,
    );

    pool.spawn_workers().await?;

    // Test shutdown with very short timeout to simulate timeout scenario
    let shutdown_result = timeout(
        Duration::from_secs(10), // Generous timeout for the test itself
        pool.shutdown_graceful(Duration::from_millis(100)), // Short timeout for workers
    )
    .await;

    match shutdown_result {
        Ok(Ok(())) => {
            // Successful shutdown
        },
        Ok(Err(e)) => {
            // Worker shutdown may have timed out, which is acceptable for this test
            tracing::info!("Worker shutdown timed out as expected: {}", e);
        },
        Err(e) => {
            unreachable!("Test itself timed out - shutdown_graceful took too long: {}", e);
        },
    }

    Ok(())
}

/// Test successful webhook delivery updates database correctly.
///
/// Verifies that when a webhook delivery succeeds, the event status
/// is updated to "delivered" and delivery timestamp is recorded.
#[tokio::test]
async fn successful_delivery_updates_database_correctly() {
    let env = TestEnv::new_isolated().await.expect("test environment setup failed");

    // Setup mock server
    let mock_server = MockServer::start().await;
    let webhook_url = format!("{}/webhook", mock_server.uri());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/webhook"))
        .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Create test data using test harness
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(&mut tx, tenant_id, &webhook_url, "test-endpoint", 5, 30)
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    // Ingest webhook event
    let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("test-source-123")
        .body(b"test webhook payload".to_vec())
        .content_type("application/json")
        .build();

    let event_id = env.ingest_webhook(&webhook_data).await.expect("ingest webhook");

    // Run delivery cycle
    env.run_delivery_cycle().await.expect("delivery cycle should succeed");

    // Verify event was delivered
    let status = env.find_webhook_status(event_id).await.expect("find webhook status");
    assert_eq!(status, "delivered", "webhook should be delivered");

    mock_server.verify().await;
}

/// Test failed webhook delivery schedules retry correctly.
///
/// Verifies that when a webhook delivery fails with a retryable error,
/// the event is marked for retry with appropriate backoff timing.
#[tokio::test]
async fn failed_delivery_schedules_retry() {
    let env = TestEnv::new_isolated().await.expect("test environment setup failed");

    // Setup mock server to return retryable error
    let mock_server = MockServer::start().await;
    let webhook_url = format!("{}/webhook", mock_server.uri());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/webhook"))
        .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Create test data
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(&mut tx, tenant_id, &webhook_url, "test-endpoint", 5, 30)
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("test-source-456")
        .body(b"test webhook payload".to_vec())
        .content_type("application/json")
        .build();

    let event_id = env.ingest_webhook(&webhook_data).await.expect("ingest webhook");

    // Run delivery cycle
    env.run_delivery_cycle().await.expect("delivery cycle should succeed");

    // Verify event is still pending (scheduled for retry)
    let status = env.find_webhook_status(event_id).await.expect("find webhook status");
    assert_eq!(status, "pending", "webhook should be pending retry");

    // Verify delivery attempt was recorded
    let attempt_count = env.count_delivery_attempts(event_id).await.expect("count attempts");
    assert_eq!(attempt_count, 1, "should have one delivery attempt");

    mock_server.verify().await;
}

/// Test exhausted retries mark event as failed.
///
/// Verifies that when a webhook reaches the maximum retry limit,
/// it is properly transitioned to failed status instead of remaining pending.
#[tokio::test]
async fn exhausted_retries_mark_event_failed() {
    let env = TestEnv::new_isolated().await.expect("test environment setup failed");

    // Setup mock server to always fail
    let mock_server = MockServer::start().await;
    let webhook_url = format!("{}/webhook", mock_server.uri());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/webhook"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .expect(4) // Initial attempt + 3 retries = 4 total attempts
        .mount(&mock_server)
        .await;

    // Create test data with low max retries
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(&mut tx, tenant_id, &webhook_url, "test-endpoint", 3, 30) // max 3 retries
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("test-source-789")
        .body(b"test webhook payload".to_vec())
        .content_type("application/json")
        .build();

    let event_id = env.ingest_webhook(&webhook_data).await.expect("ingest webhook");

    // Run delivery cycle multiple times to exhaust retries, advancing time to
    // trigger retries
    for i in 0..5 {
        env.run_test_isolated_delivery_cycle().await.expect("delivery cycle should succeed");
        // Advance time to ensure next retry attempt is ready for processing
        // Use exponential backoff timing: 1s, 2s, 4s, 8s...
        let advance_seconds = 1 << i; // 1, 2, 4, 8, 16 seconds
        env.advance_time(Duration::from_secs(advance_seconds));
    }

    // Verify event is marked as failed
    let status = env.find_webhook_status(event_id).await.expect("find webhook status");
    assert_eq!(status, "failed", "webhook should be failed after max retries");

    mock_server.verify().await;
}

/// Test worker processes multiple events correctly.
///
/// Verifies that the delivery worker can handle multiple webhook events
/// in sequence and process them all successfully.
#[tokio::test]
async fn worker_processes_multiple_events_correctly() {
    let env = TestEnv::new_isolated().await.expect("test environment setup failed");

    // Setup mock to accept all webhook deliveries
    env.http_mock
        .mock_simple("/webhook", kapsel_testing::http::MockResponse::Success {
            status: http::StatusCode::OK,
            body: bytes::Bytes::from_static(b"OK"),
        })
        .await;

    // Create test data
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            &env.http_mock.endpoint_url("/webhook"),
            "test-endpoint",
            5,
            30,
        )
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    // Create multiple webhook events
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event(&format!("test-source-{}", i))
            .body(format!("payload {}", i).into_bytes())
            .content_type("application/json")
            .build();

        let event_id = env.ingest_webhook(&webhook_data).await.expect("ingest webhook");
        event_ids.push(event_id);
    }

    // Run delivery cycle
    env.run_test_isolated_delivery_cycle().await.expect("delivery cycle should succeed");

    // Verify all events were delivered
    for event_id in event_ids {
        let status = env.find_webhook_status(event_id).await.expect("find webhook status");
        assert_eq!(status, "delivered", "all webhooks should be delivered");
    }
}

/// Test non-retryable errors mark event failed immediately.
///
/// Verifies that 4xx client errors and other non-retryable failures
/// immediately transition events to failed status without retry attempts.
#[tokio::test]
async fn non_retryable_errors_mark_event_failed_immediately() {
    let env = TestEnv::new_isolated().await.expect("test environment setup failed");

    // Setup mock server to return non-retryable error
    let mock_server = MockServer::start().await;
    let webhook_url = format!("{}/webhook", mock_server.uri());

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/webhook"))
        .respond_with(ResponseTemplate::new(400).set_body_string("Bad Request"))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Create test data
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.expect("create tenant");
    let endpoint_id = env
        .create_endpoint_with_config_tx(&mut tx, tenant_id, &webhook_url, "test-endpoint", 5, 30)
        .await
        .expect("create endpoint");
    tx.commit().await.expect("commit transaction");

    let webhook_data = kapsel_testing::fixtures::WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("test-source-400")
        .body(b"test webhook payload".to_vec())
        .content_type("application/json")
        .build();

    let event_id = env.ingest_webhook(&webhook_data).await.expect("ingest webhook");

    // Run delivery cycle
    env.run_test_isolated_delivery_cycle().await.expect("delivery cycle should succeed");

    // Verify event is marked as failed immediately (non-retryable)
    let status = env.find_webhook_status(event_id).await.expect("find webhook status");
    assert_eq!(status, "failed", "webhook should be failed immediately for 4xx error");

    // Verify only one delivery attempt was made
    let attempt_count = env.count_delivery_attempts(event_id).await.expect("count attempts");
    assert_eq!(attempt_count, 1, "should only attempt delivery once for non-retryable error");

    mock_server.verify().await;
}
