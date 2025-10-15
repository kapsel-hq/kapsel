//! Webhook ingestion integration tests.
//!
//! Tests the POST /ingest/:endpoint_id endpoint following TDD.

use chrono::{DateTime, Utc};
use insta::assert_snapshot;
use kapsel_core::TenantId;
use serde_json::json;
use test_harness::TestEnv;

/// Deterministic test data for consistent snapshot testing
struct DeterministicTestData {
    tenant_id: TenantId,
    endpoint_id: uuid::Uuid,
    event_id: uuid::Uuid,
    timestamp: DateTime<Utc>,
    host_port: String,
}

impl DeterministicTestData {
    fn new() -> Self {
        Self {
            tenant_id: TenantId(
                uuid::Uuid::parse_str("12345678-1234-5678-9abc-123456789abc").unwrap(),
            ),
            endpoint_id: uuid::Uuid::parse_str("87654321-4321-8765-cba9-876543210987").unwrap(),
            event_id: uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            timestamp: DateTime::parse_from_rfc3339("2025-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            host_port: "127.0.0.1:8080".to_string(),
        }
    }
}

#[tokio::test]
async fn webhook_ingestion_returns_200_with_event_id() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();

    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Create tenant with deterministic ID
    sqlx::query("INSERT INTO tenants (id, name, api_key) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("test-api-key")
        .execute(&pool)
        .await
        .expect("Failed to create tenant");

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
        .bind(test_data.endpoint_id)
        .bind(test_data.tenant_id.0)
        .bind("test-endpoint")
        .bind("https://example.com/webhook")
        .execute(&pool)
        .await
        .expect("Failed to insert test endpoint");

    // Set deterministic time on test clock
    env.clock.advance(std::time::Duration::from_secs(0));

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .json(&json!({
            "user_id": 123,
            "action": "user.created",
            "timestamp": "2023-01-15T10:30:00Z"
        }))
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(response.status(), 200, "Webhook ingestion should return 200 OK");

    let mut body: serde_json::Value = response.json().await.expect("Response should be valid JSON");

    // Normalize dynamic fields for deterministic snapshots
    if let Some(obj) = body.as_object_mut() {
        obj.insert("event_id".to_string(), json!(test_data.event_id.to_string()));
    }

    assert_snapshot!(serde_json::to_string_pretty(&body).unwrap());
}

#[tokio::test]
async fn webhook_ingestion_persists_to_database() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();
    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Insert tenant first to satisfy foreign key constraint
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("enterprise")
        .execute(&pool)
        .await
        .expect("Failed to insert test tenant");

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(test_data.endpoint_id)
        .bind(test_data.tenant_id.0)
        .bind("test-endpoint")
        .bind("https://example.com/webhook")
        .bind(10i32)
        .bind(30i32)
        .bind("closed")
        .execute(&pool)
        .await
        .expect("Failed to insert test endpoint");

    // Set deterministic time on test clock
    env.clock.advance(std::time::Duration::from_secs(0));

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .header("Host", &test_data.host_port)
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

    // Verify webhook persisted to database
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(uuid::Uuid::parse_str(event_id).unwrap())
        .fetch_one(&pool)
        .await
        .expect("Should query database");

    assert_eq!(count, 1, "Webhook should be persisted to database");

    // Fetch the persisted webhook event for snapshot testing with deterministic
    // data
    let mut persisted_event: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(row_to_json(webhook_events)) FROM webhook_events WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(event_id).unwrap())
    .fetch_one(&pool)
    .await
    .expect("Should fetch persisted webhook event");

    // Override dynamic fields with deterministic values for consistent snapshots
    if let Some(event_obj) = persisted_event.as_object_mut() {
        event_obj.insert("id".to_string(), json!(test_data.event_id.to_string()));
        event_obj.insert("endpoint_id".to_string(), json!(test_data.endpoint_id.to_string()));
        event_obj.insert("tenant_id".to_string(), json!(test_data.tenant_id.0.to_string()));
        event_obj.insert("received_at".to_string(), json!(test_data.timestamp.to_rfc3339()));

        // Fix headers to have deterministic host
        if let Some(headers) = event_obj.get_mut("headers").and_then(|h| h.as_object_mut()) {
            headers.insert("host".to_string(), json!(test_data.host_port));
        }
    }

    assert_snapshot!(serde_json::to_string_pretty(&persisted_event).unwrap());
}

#[tokio::test]
async fn webhook_ingestion_includes_payload_size() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();

    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Create tenant with deterministic ID
    sqlx::query("INSERT INTO tenants (id, name, api_key) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("test-api-key")
        .execute(&pool)
        .await
        .expect("Failed to create tenant");

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url) VALUES ($1, $2, $3, $4)")
        .bind(test_data.endpoint_id)
        .bind(test_data.tenant_id.0)
        .bind("test-endpoint")
        .bind("https://example.com/webhook")
        .execute(&pool)
        .await
        .expect("Failed to insert test endpoint");

    let test_payload = json!({
        "test_data": "x".repeat(1024),  // 1KB+ payload to test size calculation
        "nested": {
            "array": [1, 2, 3, 4, 5]
        }
    });

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .json(&test_payload)
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(response.status(), 200, "Webhook ingestion should succeed with payload_size");

    let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
    let event_id = body["event_id"].as_str().expect("event_id should be present");

    let stored_payload_size: i32 =
        sqlx::query_scalar("SELECT payload_size FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::parse_str(event_id).unwrap())
            .fetch_one(&pool)
            .await
            .expect("Should fetch payload_size from database");

    assert!(
        stored_payload_size > 1000,
        "Payload size should reflect actual JSON size: got {}",
        stored_payload_size
    );
}

#[tokio::test]
async fn webhook_ingestion_validates_hmac_signature_success() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();

    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Create tenant with deterministic ID
    sqlx::query("INSERT INTO tenants (id, name, api_key) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("test-api-key")
        .execute(&pool)
        .await
        .expect("Failed to create tenant");

    let signing_secret = "test_secret_key";

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, signature_header) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(test_data.endpoint_id)
            .bind(test_data.tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind(signing_secret)
            .bind("X-Webhook-Signature")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

    let payload = json!({"user_id": 123, "action": "user.created"});
    let payload_bytes = payload.to_string().into_bytes();

    let signature = kapsel_api::crypto::generate_hmac_hex(&payload_bytes, signing_secret)
        .expect("HMAC generation should succeed in test");

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .header("X-Webhook-Signature", format!("sha256={}", signature))
        .json(&payload)
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(response.status(), 200, "Valid signature should be accepted");

    let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
    let event_id = body["event_id"].as_str().expect("event_id should be present");

    let signature_valid: Option<bool> =
        sqlx::query_scalar("SELECT signature_valid FROM webhook_events WHERE id = $1")
            .bind(uuid::Uuid::parse_str(event_id).unwrap())
            .fetch_one(&pool)
            .await
            .expect("Should fetch signature validation result");

    assert_eq!(signature_valid, Some(true), "Valid signature should be stored as true");
}

#[tokio::test]
async fn webhook_ingestion_rejects_invalid_hmac_signature() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();

    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Create tenant with deterministic ID
    sqlx::query("INSERT INTO tenants (id, name, api_key) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("test-api-key")
        .execute(&pool)
        .await
        .expect("Failed to create tenant");

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret, signature_header) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(test_data.endpoint_id)
            .bind(test_data.tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind("test_secret_key")
            .bind("X-Webhook-Signature")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

    let payload = json!({"user_id": 123, "action": "user.created"});

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .header("X-Webhook-Signature", "sha256=invalid_signature_here")
        .json(&payload)
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(response.status(), 400, "Invalid signature should be rejected with 400");

    let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("signature"),
        "Error should mention signature validation"
    );
}

#[tokio::test]
async fn webhook_ingestion_requires_signature_when_configured() {
    let env = TestEnv::new().await.expect("Failed to create test environment");
    let pool = env.db.pool();

    // Setup deterministic test data
    let test_data = DeterministicTestData::new();

    let addr = "127.0.0.1:0";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("Failed to bind");
    let actual_addr = listener.local_addr().expect("Failed to get local addr");

    let db_clone = pool.clone();
    tokio::spawn(async move {
        let app = kapsel_api::create_router(db_clone);
        axum::serve(listener, app).await.expect("Server failed");
    });

    // Create tenant with deterministic ID
    sqlx::query("INSERT INTO tenants (id, name, api_key) VALUES ($1, $2, $3)")
        .bind(test_data.tenant_id.0)
        .bind("test-tenant")
        .bind("test-api-key")
        .execute(&pool)
        .await
        .expect("Failed to create tenant");

    sqlx::query("INSERT INTO endpoints (id, tenant_id, name, url, signing_secret) VALUES ($1, $2, $3, $4, $5)")
            .bind(test_data.endpoint_id)
            .bind(test_data.tenant_id.0)
            .bind("test-endpoint")
            .bind("https://example.com/webhook")
            .bind("test_secret_key")
            .execute(&pool)
            .await
            .expect("Failed to insert test endpoint");

    let payload = json!({"user_id": 123, "action": "user.created"});

    let response = env
        .client
        .post(format!("http://{}/ingest/{}", actual_addr, test_data.endpoint_id))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(
        response.status(),
        400,
        "Missing signature should be rejected when endpoint requires it"
    );

    let body: serde_json::Value = response.json().await.expect("Response should be valid JSON");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("signature"),
        "Error should mention missing signature"
    );
}
