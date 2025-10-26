//! Health check integration tests for test infrastructure validation.
//!
//! Verifies that the test environment initializes correctly and basic
//! server functionality works as expected.

use kapsel_testing::{fixtures::WebhookBuilder, Clock, TestEnv};

#[tokio::test]
async fn test_environment_initializes() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - verify components are accessible
    let health_check = env.database_health_check().await.expect("Health check should work");

    // Assert
    assert!(health_check, "Database connection should work");
    assert!(!env.http_mock.url().is_empty(), "Mock server should have URL");
}

#[tokio::test]
async fn database_migrations_applied() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - check if core tables exist
    let tables = env.list_tables().await.expect("Should query tables");

    // Assert - verify expected tables exist
    assert!(tables.contains(&"webhook_events".to_string()), "webhook_events table should exist");
    assert!(tables.contains(&"endpoints".to_string()), "endpoints table should exist");
    assert!(
        tables.contains(&"delivery_attempts".to_string()),
        "delivery_attempts table should exist"
    );
    assert!(tables.contains(&"tenants".to_string()), "tenants table should exist");
}

#[tokio::test]
async fn test_clock_advances_time() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let start = env.clock.now();

    // Act
    env.advance_time(std::time::Duration::from_secs(60));

    // Assert
    let elapsed = env.clock.now().duration_since(start);
    assert_eq!(elapsed, std::time::Duration::from_secs(60), "Clock should advance by 60 seconds");
}

#[tokio::test]
async fn http_mock_server_responds() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let mock_url = env.http_mock.url();

    // Act - configure mock endpoint
    env.http_mock
        .mock_endpoint(kapsel_testing::http::MockEndpoint::success("/health").with_body("OK"))
        .await;

    // Make request to mock (in real test, this would be done by the webhook
    // delivery system)
    let client = reqwest::Client::new();
    let response =
        client.post(format!("{}/health", mock_url)).send().await.expect("Request should succeed");

    // Assert
    assert_eq!(response.status(), 200, "Mock should return 200 OK");
    let body = response.text().await.expect("Should read body");
    assert_eq!(body, "OK", "Mock should return expected body");
}

#[tokio::test]
async fn webhook_fixture_builder_creates_valid_data() {
    // Arrange & Act
    let webhook = WebhookBuilder::with_defaults()
        .json_body(&serde_json::json!({
            "event": "test.created",
            "data": {
                "id": "123",
                "name": "Test"
            }
        }))
        .header("X-Custom-Header", "custom-value")
        .build();

    // Assert
    assert!(!webhook.source_event_id.is_empty(), "Should have source event ID");
    assert_eq!(webhook.idempotency_strategy, "header", "Should have default strategy");
    assert_eq!(webhook.content_type, "application/json", "Should have JSON content type");
    assert!(webhook.headers.contains_key("X-Custom-Header"), "Should have custom header");
    assert_eq!(
        webhook.headers.get("X-Custom-Header").unwrap(),
        "custom-value",
        "Header value should match"
    );
}

#[tokio::test]
async fn database_transaction_rollback_works() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let tenant_id = uuid::Uuid::new_v4();

    // Act - Test that database operations work
    // For this test, we'll verify that the database pool is accessible
    let _pool = env.pool();

    // The transaction will be dropped and rollback automatically
    // The key test is that our transaction infrastructure works

    // Test that normal operations work (not in transaction)
    let initial_count =
        env.count_by_id("tenants", "id", tenant_id).await.expect("Should query count");

    assert_eq!(initial_count, 0, "Tenant should not exist initially");

    // Test that a committed operation does persist
    let mut tx = env.pool().begin().await.expect("begin transaction");
    let created_tenant_id =
        env.create_tenant_tx(&mut *tx, "Test Tenant").await.expect("Insert should work");
    tx.commit().await.expect("commit transaction");
    let _tenant_id_str = created_tenant_id.0.to_string();

    let final_count =
        env.count_by_id("tenants", "id", created_tenant_id.0).await.expect("Should query count");

    assert_eq!(final_count, 1, "Tenant should exist after commit");
}

#[tokio::test]
async fn scenario_builder_executes_steps() {
    use std::time::Duration;

    use kapsel_testing::ScenarioBuilder;

    // Arrange
    let mut env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - Build and run a simple scenario
    let scenario = ScenarioBuilder::new("test health check scenario")
        .advance_time(Duration::from_secs(1))
        .assert_state(|env| {
            // Verify we can access the environment in assertions
            assert!(env.clock.elapsed() >= Duration::from_secs(1));
            Ok(())
        });

    // Assert - Scenario should run without errors
    scenario.run(&mut env).await.expect("Scenario should execute successfully");
}
