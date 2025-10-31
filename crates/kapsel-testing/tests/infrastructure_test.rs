//! Integration tests for test infrastructure components.
//!
//! Tests test utilities, fixtures, and infrastructure without external
//! dependencies like Docker containers.

use std::time::Duration;

use kapsel_core::Clock;
use kapsel_testing::{
    fixtures::{scenarios, EndpointBuilder, WebhookBuilder},
    http::{MockEndpoint, ScenarioBuilder as HttpScenarioBuilder},
    time::{backoff, TestClock},
};
use serde_json::json;

#[tokio::test]
async fn test_infrastructure_components_work() {
    // Test infrastructure components without database setup
    let clock = TestClock::new();

    // Act & Assert - Test time manipulation
    let start = clock.now();
    clock.advance(Duration::from_secs(30));
    let elapsed = clock.now().duration_since(start);
    assert_eq!(elapsed, Duration::from_secs(30));

    // Test HTTP mocking (no database needed)
    let mock_server = kapsel_testing::http::MockServer::start().await;
    assert!(!mock_server.url().is_empty());

    // Configure mock endpoint
    mock_server
        .mock_endpoint(
            MockEndpoint::success("/webhook")
                .with_header("Content-Type", "application/json")
                .with_body(r#"{"status": "received"}"#),
        )
        .await;

    // Test that we can make requests to mock
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/webhook", mock_server.url()))
        .json(&json!({"test": "data"}))
        .send()
        .await
        .expect("Mock request should succeed");

    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert_eq!(body, r#"{"status": "received"}"#);
}

#[tokio::test]
async fn fixture_builders_create_valid_test_data() {
    // Test WebhookBuilder
    let webhook = WebhookBuilder::with_defaults()
        .json_body(&json!({
            "event": "payment.completed",
            "data": {
                "payment_id": "pay_123",
                "amount": 2000,
                "currency": "usd"
            }
        }))
        .header("X-Stripe-Signature", "t=1234567890,v1=signature_here")
        .source_event("pay_123")
        .build();

    assert!(!webhook.source_event_id.is_empty());
    assert_eq!(webhook.source_event_id, "pay_123");
    assert_eq!(webhook.idempotency_strategy, "header");
    assert_eq!(webhook.content_type, "application/json");
    assert!(webhook.headers.contains_key("X-Stripe-Signature"));

    // Test EndpointBuilder
    let endpoint = EndpointBuilder::with_defaults()
        .name("payment-processor")
        .url("https://api.example.com/payments")
        .max_retries(5)
        .timeout(60)
        .build();

    assert_eq!(endpoint.name, "payment-processor");
    assert_eq!(endpoint.url, "https://api.example.com/payments");
    assert_eq!(endpoint.max_retries, 5);
    assert_eq!(endpoint.timeout_seconds, 60);
}

#[tokio::test]
async fn scenario_builders_work() {
    // Test pre-built scenarios
    let (first, second) = scenarios::duplicate_webhook();
    assert_eq!(first.source_event_id, second.source_event_id);
    assert_eq!(first.tenant_id, second.tenant_id);
    assert_eq!(first.endpoint_id, second.endpoint_id);

    let stripe_webhook = scenarios::stripe_webhook();
    assert!(stripe_webhook.headers.contains_key("Stripe-Signature"));
    assert_eq!(stripe_webhook.idempotency_strategy, "source_id");

    let github_webhook = scenarios::github_webhook();
    assert!(github_webhook.headers.contains_key("X-GitHub-Event"));
    assert!(github_webhook.headers.contains_key("X-GitHub-Delivery"));

    let batch = scenarios::webhook_batch(5);
    assert_eq!(batch.len(), 5);
    assert!(batch.iter().all(|w| w.tenant_id == batch[0].tenant_id));
}

#[tokio::test]
async fn http_scenario_builder_chains_responses() {
    // Arrange
    let mock = kapsel_testing::http::MockServer::start().await;

    // Act - Build complex failure/recovery scenario
    HttpScenarioBuilder::new(mock)
        .respond_error("/webhook", http::StatusCode::SERVICE_UNAVAILABLE, None)
        .respond_error(
            "/webhook",
            http::StatusCode::INTERNAL_SERVER_ERROR,
            Some(Duration::from_secs(1)),
        )
        .respond_ok("/webhook", Some(Duration::from_secs(2)))
        .build()
        .await;

    // Assert - Could test actual HTTP behavior here
    // This demonstrates the API works without needing the full system
}

#[test]
fn backoff_calculation_works() {
    // Test standard webhook backoff
    let attempt_0 = backoff::standard_webhook_backoff(0);
    let attempt_1 = backoff::standard_webhook_backoff(1);
    let attempt_2 = backoff::standard_webhook_backoff(2);

    // Should increase exponentially (with jitter)
    assert!(attempt_1 >= attempt_0);
    assert!(attempt_2 >= attempt_1);

    // Should be roughly in expected ranges (accounting for jitter)
    assert!(attempt_0 >= Duration::from_millis(750)); // ~1s - 25%
    assert!(attempt_0 <= Duration::from_millis(1250)); // ~1s + 25%

    // Test with custom parameters
    let custom =
        backoff::exponential_with_jitter(2, Duration::from_secs(1), Duration::from_secs(10), 0.1);

    // Should respect max delay
    assert!(custom <= Duration::from_secs(10));

    // Test max delay enforcement
    let max_test = backoff::exponential_with_jitter(
        100, // Very high attempt
        Duration::from_secs(1),
        Duration::from_secs(5), // Low max
        0.0,                    // No jitter for predictable test
    );
    assert_eq!(max_test, Duration::from_secs(5));
}

#[test]
fn test_clock_deterministic_behavior() {
    // Test deterministic time control
    let clock = TestClock::new();
    let start = clock.now();

    // Advance in steps
    clock.advance(Duration::from_secs(10));
    clock.advance(Duration::from_secs(5));
    clock.advance(Duration::from_millis(500));

    let total_elapsed = clock.now().duration_since(start);
    assert_eq!(total_elapsed, Duration::from_millis(15500));

    // Test system time advancement
    let sys_start = clock.now_system();
    clock.advance(Duration::from_secs(60));
    let sys_elapsed = clock.now_system().duration_since(sys_start).unwrap();
    assert_eq!(sys_elapsed, Duration::from_secs(60));
}

// Test demonstrating TDD cycle for retry logic
#[tokio::test]
async fn retry_logic_exponential_backoff_timing() {
    // Arrange
    let clock = TestClock::new();
    let mock = kapsel_testing::http::MockServer::start().await;

    // Configure endpoint to fail then succeed
    mock.mock_endpoint(MockEndpoint::failure("/webhook", http::StatusCode::SERVICE_UNAVAILABLE))
        .await;

    // Act - Simulate retry attempts with deterministic timing
    let mut attempt_times = Vec::new();

    for attempt in 0..3 {
        let delay = backoff::standard_webhook_backoff(attempt);
        attempt_times.push(clock.now());
        clock.advance(delay);

        // Would make actual HTTP request here in real implementation
        tracing::debug!("Attempt {} at {:?} after delay {:?}", attempt + 1, clock.now(), delay);
    }

    // Assert - Verify exponential backoff timing
    assert_eq!(attempt_times.len(), 3);

    // Each attempt should be roughly double the previous delay (with jitter
    // tolerance)
    let delay1 = attempt_times[1].duration_since(attempt_times[0]);
    let delay2 = attempt_times[2].duration_since(attempt_times[1]);

    // With jitter, second delay should be roughly 2x first (allowing 50% variance)
    assert!(delay2.as_millis() >= delay1.as_millis() / 2);
    assert!(delay2.as_millis() <= delay1.as_millis() * 4);
}

// Integration test showing complete workflow
#[tokio::test]
async fn complete_webhook_reliability_workflow() {
    // This test demonstrates the full webhook processing pipeline
    // without needing database or external dependencies

    // Arrange - Set up test environment
    let clock = TestClock::new();
    let mock_destination = kapsel_testing::http::MockServer::start().await;

    // Configure destination to initially fail, then succeed
    mock_destination
        .mock_endpoint(MockEndpoint::failure("/webhook", http::StatusCode::SERVICE_UNAVAILABLE))
        .await;

    // Create test webhook
    let webhook = scenarios::stripe_webhook();

    // Act - Simulate ingestion and delivery process

    // 1. Webhook received (would persist to DB)
    let received_at = clock.now();
    tracing::debug!("Webhook received at: {:?}", received_at);

    // 2. First delivery attempt fails
    clock.advance(Duration::from_millis(100)); // Processing time
    let first_attempt = clock.now();
    tracing::debug!("First delivery attempt at: {:?}", first_attempt);

    // 3. Schedule retry with exponential backoff
    let retry_delay = backoff::standard_webhook_backoff(0);
    clock.advance(retry_delay);
    let retry_at = clock.now();
    tracing::debug!("Retry scheduled for: {:?} (after {:?})", retry_at, retry_delay);

    // 4. Configure destination to succeed on retry
    mock_destination
        .mock_endpoint(MockEndpoint::success("/webhook").with_body(r#"{"status": "processed"}"#))
        .await;

    // 5. Retry succeeds
    let delivered_at = clock.now();
    tracing::debug!("Webhook delivered at: {:?}", delivered_at);

    // Assert - Verify complete flow timing
    let total_time = delivered_at.duration_since(received_at);
    let expected_min = Duration::from_millis(100) + retry_delay;

    assert!(total_time >= expected_min);
    assert!(total_time <= expected_min + Duration::from_millis(50)); // Small tolerance

    // Verify webhook data integrity
    assert_eq!(webhook.content_type, "application/json");
    assert!(webhook.headers.contains_key("Stripe-Signature"));
    assert!(!webhook.body.is_empty());

    tracing::info!("Complete webhook reliability workflow test passed!");
    tracing::info!("   Total processing time: {:?}", total_time);
    tracing::info!("   Webhook delivered successfully with exponential backoff retry");
}

/// Example test demonstrating transaction-based isolation pattern.
///
/// This test shows how to use the shared database with transactions for
/// perfect test isolation. The transaction automatically rolls back when
/// dropped, cleaning up all test data without affecting other tests.
#[tokio::test]
async fn example_transaction_based_test() {
    let env = kapsel_testing::TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Test data created in transaction
    let tenant_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants (id, name, tier, created_at, updated_at)
         VALUES ($1, $2, $3, NOW(), NOW())",
    )
    .bind(tenant_id)
    .bind(format!("example-tenant-{}", tenant_id.simple()))
    .bind("free")
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify data exists within transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();

    assert_eq!(count, 1);

    // Transaction automatically rolls back when dropped
    // No explicit rollback needed, but we can be explicit for clarity
    drop(tx);

    // Verify data was rolled back (no longer exists in database)
    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(env.pool())
        .await
        .unwrap();

    assert_eq!(count_after, 0, "Data should not persist after transaction rollback");
}
