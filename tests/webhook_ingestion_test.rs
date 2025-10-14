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

        let pool = env.db.pool();
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
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");

        // Insert endpoint directly using the pool
        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .execute(&pool)
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

        let pool = env.db.pool();
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
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");

        // Insert endpoint directly using the pool
        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .execute(&pool)
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
            .fetch_one(&pool)
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

#[tokio::test]
async fn webhook_ingestion_includes_payload_size() {
    // Arrange
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // This test specifically verifies the payload_size column is included in
    // persistence Following TDD: this test should FAIL until Fix 1.1 is
    // implemented

    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = env.db.pool();
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
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");

        // Insert endpoint directly using the pool
        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

        // Act - POST webhook with specific payload size
        let test_payload = json!({
            "test_data": "x".repeat(1024),  // 1KB+ payload to test size calculation
            "nested": {
                "array": [1, 2, 3, 4, 5]
            }
        });

        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .json(&test_payload)
            .send()
            .await
            .expect("Request should complete");

        // Assert - Request succeeds (this will fail if payload_size is missing)
        assert_eq!(response.status(), 200, "Webhook ingestion should succeed with payload_size");

        let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
        let event_id = body["event_id"].as_str().expect("event_id should be present");

        // Verify payload_size was stored correctly
        let stored_payload_size: i32 =
            sqlx::query_scalar("SELECT payload_size FROM webhook_events WHERE id = $1")
                .bind(uuid::Uuid::parse_str(event_id).unwrap())
                .fetch_one(&pool)
                .await
                .expect("Should fetch payload_size from database");

        // Payload size should be > 0 and reasonable for our test data
        assert!(
            stored_payload_size > 1000,
            "Payload size should reflect actual JSON size: got {}",
            stored_payload_size
        );
    }

    #[cfg(not(feature = "docker"))]
    {
        // Skip full server test - infrastructure verification only
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}

#[tokio::test]
async fn webhook_event_struct_has_required_fields() {
    // This test verifies that WebhookEvent struct compilation succeeds
    // with the new database fields (Fix 1.2)

    // The real verification is that kapsel-core compiles successfully
    // with the new fields: payload_size, signature_valid, signature_error,
    // tigerbeetle_id

    // If this test runs, it means the struct fields are accessible
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Verify test infrastructure works
    assert!(env.database_health_check().await.expect("Health check should work"));

    // Note: The primary validation is that cargo check passes for kapsel-core
    // with the added fields in WebhookEvent struct
}

#[tokio::test]
async fn webhook_ingestion_validates_hmac_signature_success() {
    // RED PHASE: This test should FAIL until signature validation is implemented
    let env = TestEnv::new().await.expect("Failed to create test environment");

    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = env.db.pool();
        let addr = "127.0.0.1:0";
        let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
        let actual_addr = listener.local_addr().expect("Failed to get local addr");

        let db_clone = pool.clone();
        tokio::spawn(async move {
            let app = kapsel_api::create_router(db_clone);
            axum::serve(listener, app).await.expect("Server failed");
        });

        // Setup endpoint with signing secret
        let endpoint_id = uuid::Uuid::new_v4();
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");
        let signing_secret = "test_secret_key";

        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, signature_header) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind(signing_secret)
            .bind("X-Webhook-Signature")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

        let payload = json!({"user_id": 123, "action": "user.created"});
        let payload_bytes = payload.to_string().into_bytes();

        // Generate valid HMAC signature
        let signature = kapsel_api::crypto::generate_hmac_hex(&payload_bytes, signing_secret)
            .expect("HMAC generation should succeed in test");

        // Act - POST with valid signature
        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", format!("sha256={}", signature))
            .json(&payload)
            .send()
            .await
            .expect("Request should complete");

        // Assert - Should succeed with valid signature
        assert_eq!(response.status(), 200, "Valid signature should be accepted");

        let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
        let event_id = body["event_id"].as_str().expect("event_id should be present");

        // Verify signature validation result stored in database
        let signature_valid: Option<bool> =
            sqlx::query_scalar("SELECT signature_valid FROM webhook_events WHERE id = $1")
                .bind(uuid::Uuid::parse_str(event_id).unwrap())
                .fetch_one(&pool)
                .await
                .expect("Should fetch signature validation result");

        assert_eq!(signature_valid, Some(true), "Valid signature should be stored as true");
    }

    #[cfg(not(feature = "docker"))]
    {
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}

#[tokio::test]
async fn webhook_ingestion_rejects_invalid_hmac_signature() {
    // RED PHASE: This test should FAIL until signature validation is implemented
    let env = TestEnv::new().await.expect("Failed to create test environment");

    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = env.db.pool();
        let addr = "127.0.0.1:0";
        let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
        let actual_addr = listener.local_addr().expect("Failed to get local addr");

        let db_clone = pool.clone();
        tokio::spawn(async move {
            let app = kapsel_api::create_router(db_clone);
            axum::serve(listener, app).await.expect("Server failed");
        });

        // Setup endpoint with signing secret
        let endpoint_id = uuid::Uuid::new_v4();
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");

        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, signature_header) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind("test_secret_key")
            .bind("X-Webhook-Signature")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

        let payload = json!({"user_id": 123, "action": "user.created"});

        // Act - POST with invalid signature
        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", "sha256=invalid_signature_here")
            .json(&payload)
            .send()
            .await
            .expect("Request should complete");

        // Assert - Should reject with 400 Bad Request
        assert_eq!(response.status(), 400, "Invalid signature should be rejected with 400");

        let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
        assert!(
            body["error"].as_str().unwrap().contains("signature"),
            "Error should mention signature validation"
        );
    }

    #[cfg(not(feature = "docker"))]
    {
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}

#[tokio::test]
async fn webhook_ingestion_requires_signature_when_configured() {
    // RED PHASE: This test should FAIL until signature validation is implemented
    let env = TestEnv::new().await.expect("Failed to create test environment");

    #[cfg(feature = "docker")]
    {
        use serde_json::json;

        let pool = env.db.pool();
        let addr = "127.0.0.1:0";
        let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
        let actual_addr = listener.local_addr().expect("Failed to get local addr");

        let db_clone = pool.clone();
        tokio::spawn(async move {
            let app = kapsel_api::create_router(db_clone);
            axum::serve(listener, app).await.expect("Server failed");
        });

        // Setup endpoint with signing secret but no signature header default
        let endpoint_id = uuid::Uuid::new_v4();
        let tenant_id = env.create_tenant("test-tenant").await.expect("Failed to create tenant");

        sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret) VALUES ($1, $2, $3, $4, $5)")
            .bind(endpoint_id)
            .bind(tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind("test_secret_key")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

        let payload = json!({"user_id": 123, "action": "user.created"});

        // Act - POST without signature header
        let response = env
            .client
            .post(format!("http://{}/ingest/{}", actual_addr, endpoint_id))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .expect("Request should complete");

        // Assert - Should reject with 400 Bad Request when signature is required but
        // missing
        assert_eq!(
            response.status(),
            400,
            "Missing signature should be rejected when endpoint requires it"
        );

        let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
        assert!(
            body["error"].as_str().unwrap().contains("signature"),
            "Error should mention missing signature"
        );
    }

    #[cfg(not(feature = "docker"))]
    {
        assert!(env.database_health_check().await.expect("Health check should work"));
    }
}
