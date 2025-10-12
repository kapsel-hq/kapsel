//! Health check integration tests.
//!
//! Verifies that the test infrastructure and basic server functionality work correctly.

use test_harness::{fixtures::WebhookBuilder, Clock, TestEnv};

#[tokio::test]
async fn test_environment_initializes() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - verify components are accessible
    let db_result = sqlx::query("SELECT 1 as health").fetch_one(&env.db).await;

    // Assert
    assert!(db_result.is_ok(), "Database connection should work");
    assert!(!env.http_mock.url().is_empty(), "Mock server should have URL");
}

#[tokio::test]
async fn database_migrations_applied() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - check if core tables exist
    let tables_query = r#"
        SELECT table_name
        FROM information_schema.tables
        WHERE table_schema = 'public'
        AND table_type = 'BASE TABLE'
        ORDER BY table_name
    "#;

    let tables: Vec<String> =
        sqlx::query_scalar(tables_query).fetch_all(&env.db).await.expect("Should query tables");

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
        .mock_endpoint(test_harness::http::MockEndpoint::success("/health").with_body("OK"))
        .await;

    // Make request to mock (in real test, this would be done by the webhook delivery system)
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
        .json_body(serde_json::json!({
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

    // Create a test tenant
    let tenant_id = uuid::Uuid::new_v4();

    // Act - Insert in transaction that will rollback
    {
        let _tx = env.transaction().await.expect("Should create transaction");

        sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind("free")
            .execute(&env.db)
            .await
            .expect("Insert should work in transaction");

        // Transaction drops here, causing rollback
    }

    // Assert - Tenant should not exist after rollback
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(&env.db)
        .await
        .expect("Should query count");

    assert_eq!(count, 0, "Tenant should not exist after transaction rollback");
}

#[tokio::test]
async fn scenario_builder_executes_steps() {
    use std::time::Duration;
    use test_harness::ScenarioBuilder;

    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - Build and run a simple scenario
    let scenario = ScenarioBuilder::new("test health check scenario")
        .advance_time(Duration::from_secs(1))
        .assert_state(|env| {
            // Verify we can access the environment in assertions
            assert!(env.clock.elapsed() >= Duration::from_secs(1));
            Ok(())
        });

    // Assert - Scenario should run without errors
    scenario.run(&env).await.expect("Scenario should execute successfully");
}

// This test will initially fail (RED phase of TDD)
// Uncomment when implementing the health endpoint
/*
#[tokio::test]
async fn health_endpoint_returns_ok() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Act - Call health endpoint (this will fail initially - RED phase)
    let response = env.client
        .get("/health")
        .send()
        .await
        .expect("Request should complete");

    // Assert
    assert_eq!(response.status(), 200, "Health endpoint should return 200 OK");

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");
    assert_eq!(body["status"], "healthy", "Status should be healthy");
    assert!(body["database"].as_bool().unwrap_or(false), "Database should be healthy");
}
*/
