//! Webhook ingestion integration tests.
//!
//! Tests the POST /ingest/:endpoint_id endpoint following TDD.

use test_harness::TestEnv;

#[tokio::test]
async fn webhook_ingestion_returns_200_with_event_id() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Only run full server test with PostgreSQL backend
    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = &env.db;
        let addr = "127.0.0.1:0";
        let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
        let actual_addr = listener.local_addr().expect("Failed to get local addr");

        let db_clone = pool.clone();
        tokio::spawn(async move {
            let app = kapsel_api::create_router(db_clone);
            axum::serve(listener, app).await.expect("Server failed");
        });

        // Setup test data
        let endpoint_id = uuid::Uuid::new_v4();
        let tenant_id =
            env.insert_test_tenant("test-tenant", "free").await.expect("Failed to create tenant");

        // Insert endpoint directly using the pool
        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
            .bind(endpoint_id)
            .bind(uuid::Uuid::parse_str(&tenant_id).unwrap())
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .execute(pool)
            .await
            .expect("Failed to insert test endpoint");

        // Act - POST webhook to ingestion endpoint
        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .json(&json!({
                "user_id": 123,
                "action": "user.created",
                "timestamp": "2023-01-15T10:30:00Z"
            }))
            .send()
            .await
            .expect("Request should complete");

        // Assert
        assert_eq!(response.status(), 200, "Webhook ingestion should return 200 OK");

        let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");

        assert!(body["event_id"].is_string(), "Response should include event_id");
        assert_eq!(body["status"], "received", "Response should show status as received");
    }

    #[cfg(not(feature = "docker"))]
    {
        // Skip full server test - infrastructure verification only
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}

#[tokio::test]
async fn webhook_ingestion_persists_to_database() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Only run full server test with PostgreSQL backend
    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = &env.db;
        let addr = "127.0.0.1:0";
        let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
        let actual_addr = listener.local_addr().expect("Failed to get local addr");

        let db_clone = pool.clone();
        tokio::spawn(async move {
            let app = kapsel_api::create_router(db_clone);
            axum::serve(listener, app).await.expect("Server failed");
        });

        // Setup test data
        let endpoint_id = uuid::Uuid::new_v4();
        let tenant_id =
            env.insert_test_tenant("test-tenant", "free").await.expect("Failed to create tenant");

        // Insert endpoint directly using the pool
        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
            .bind(endpoint_id)
            .bind(uuid::Uuid::parse_str(&tenant_id).unwrap())
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .execute(pool)
            .await
            .expect("Failed to insert test endpoint");

        // Act - POST webhook
        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .json(&json!({
                "order_id": "ord_123",
                "status": "completed"
            }))
            .send()
            .await
            .expect("Request should complete");

        assert_eq!(response.status(), 200);

        let body: serde_json::Value = response.json().await.unwrap();
        let event_id = body["event_id"].as_str().expect("event_id should be present");

        // Assert - Verify webhook persisted to database
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::parse_str(event_id).unwrap())
            .fetch_one(pool)
            .await
            .expect("Should query database");

        assert_eq!(count, 1, "Webhook should be persisted to database");
    }

    #[cfg(not(feature = "docker"))]
    {
        // Skip full server test - infrastructure verification only
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}
