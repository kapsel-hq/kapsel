//! Webhook ingestion integration tests.
//!
//! Tests the POST /ingest/:endpoint_id endpoint following TDD.

use serde_json::json;
use test_harness::TestEnv;

#[tokio::test]
async fn webhook_ingestion_returns_200_with_event_id() {
    // Arrange
    let mut env = TestEnv::new().await.expect("Failed to create test environment");

    // Start server
    let db = env.db.clone();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    tokio::spawn(async move {
        let app = hooky_api::create_router(db);
        axum::serve(listener, app).await.expect("Server failed");
    });

    env.with_server(actual_addr);

    // Create test endpoint in database
    let endpoint_id = uuid::Uuid::new_v4();
    let tenant_id = uuid::Uuid::new_v4();

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .execute(&env.db)
    .await
    .expect("Failed to insert test endpoint");

    // Act - POST webhook to ingestion endpoint
    let response = env
        .client
        .post(format!("{}/ingest/{}", env.base_url(), endpoint_id))
        .header("Content-Type", "application/json")
        .header("X-Idempotency-Key", "test-key-123")
        .json(&json!({
            "event": "test.webhook",
            "data": {
                "id": "123",
                "message": "Hello webhook!"
            }
        }))
        .send()
        .await
        .expect("Request should complete");

    // Assert
    assert_eq!(
        response.status(),
        200,
        "Webhook ingestion should return 200 OK"
    );

    let body: serde_json::Value = response
        .json()
        .await
        .expect("Response should be valid JSON");

    assert!(
        body["event_id"].is_string(),
        "Response should include event_id"
    );
    assert_eq!(
        body["status"],
        "received",
        "Response should show status as received"
    );
}

#[tokio::test]
async fn webhook_ingestion_persists_to_database() {
    // Arrange
    let mut env = TestEnv::new().await.expect("Failed to create test environment");

    // Start server
    let db = env.db.clone();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    tokio::spawn(async move {
        let app = hooky_api::create_router(db);
        axum::serve(listener, app).await.expect("Server failed");
    });

    env.with_server(actual_addr);

    let endpoint_id = uuid::Uuid::new_v4();
    let tenant_id = uuid::Uuid::new_v4();

    sqlx::query(
        "INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)",
    )
    .bind(endpoint_id)
    .bind(tenant_id)
    .bind("test-endpoint")
    .bind("https://example.com/webhook")
    .execute(&env.db)
    .await
    .expect("Failed to insert test endpoint");

    // Act - POST webhook
    let response = env
        .client
        .post(format!("{}/ingest/{}", env.base_url(), endpoint_id))
        .header("Content-Type", "application/json")
        .json(&json!({"event": "test"}))
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.unwrap();
    let event_id = body["event_id"]
        .as_str()
        .expect("event_id should be present");

    // Assert - Verify webhook persisted to database
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(uuid::Uuid::parse_str(event_id).unwrap())
        .fetch_one(&env.db)
        .await
        .expect("Should query database");

    assert_eq!(count, 1, "Webhook should be persisted to database");
}
