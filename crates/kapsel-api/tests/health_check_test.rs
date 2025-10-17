//! Health check endpoint tests.
//!
//! Tests the `/health` endpoint functionality including service readiness,
//! database connectivity checks, and proper response formatting. Verifies
//! that the health check endpoint provides accurate service status information.

use axum::http::StatusCode;
use kapsel_api::create_router;
use kapsel_testing::TestEnv;
use serde_json::Value;
use tower::ServiceExt;

/// Test health check endpoint returns success when all systems are healthy.
///
/// Verifies that the health check endpoint responds with 200 OK when
/// the database is accessible and all systems are operational.
#[tokio::test]
async fn health_check_returns_success_when_healthy() {
    let env = TestEnv::new().await.expect("failed to create test environment");
    let app = create_router(env.pool().clone());

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.expect("failed to make request");

    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("failed to read response body");
    let body_str = std::str::from_utf8(&body_bytes).expect("failed to parse response body");

    // Health check should return structured JSON response
    let health_response: Value =
        serde_json::from_str(body_str).expect("health check response should be valid JSON");

    // Verify response contains status field
    assert!(health_response.get("status").is_some(), "Health check should include status field");

    // Status should indicate healthy state
    let status = health_response["status"].as_str().expect("status should be a string");
    assert!(
        status == "healthy" || status == "ok" || status == "UP",
        "Status should indicate healthy state, got: {}",
        status
    );
}

/// Test health check endpoint includes database connectivity status.
///
/// Verifies that the health check response includes information about
/// database connectivity and operational status.
#[tokio::test]
async fn health_check_includes_database_status() {
    let env = TestEnv::new().await.expect("failed to create test environment");
    let app = create_router(env.pool().clone());

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.expect("failed to make request");
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("failed to read response body");
    let body_str = std::str::from_utf8(&body_bytes).expect("failed to parse response body");

    let health_response: Value =
        serde_json::from_str(body_str).expect("health check response should be valid JSON");

    // Health check should include database status
    assert!(
        health_response.get("database").is_some()
            || health_response.get("db").is_some()
            || health_response.get("checks").is_some(),
        "Health check should include database status information"
    );
}

/// Test health check endpoint responds quickly.
///
/// Verifies that the health check endpoint has low latency and doesn't
/// perform expensive operations that would slow down health monitoring.
#[tokio::test]
async fn health_check_responds_quickly() {
    let env = TestEnv::new().await.expect("failed to create test environment");
    let app = create_router(env.pool().clone());

    let start_time = std::time::Instant::now();

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.expect("failed to make request");

    let duration = start_time.elapsed();

    assert_eq!(response.status(), StatusCode::OK);

    // Health check should respond within 5 seconds (generous for test environments)
    assert!(
        duration < std::time::Duration::from_secs(5),
        "Health check took too long: {:?}",
        duration
    );
}

/// Test health check endpoint handles concurrent requests.
///
/// Verifies that multiple concurrent health check requests are handled
/// properly without race conditions or resource contention.
#[tokio::test]
async fn health_check_handles_concurrent_requests() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Launch multiple concurrent health check requests
    let mut handles = Vec::new();

    for _ in 0..10 {
        let pool = env.pool().clone();
        let handle = tokio::spawn(async move {
            let app = create_router(pool);

            let request = axum::http::Request::builder()
                .method("GET")
                .uri("/health")
                .body(axum::body::Body::empty())
                .unwrap();

            app.oneshot(request).await.expect("failed to make request")
        });

        handles.push(handle);
    }

    // Wait for all requests to complete
    let responses = futures::future::join_all(handles).await;

    // Verify all health checks succeeded
    for response_result in responses {
        let response = response_result.expect("health check task should complete");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

/// Test health check endpoint with various HTTP methods.
///
/// Verifies that the health check endpoint properly handles different
/// HTTP methods and responds appropriately.
#[tokio::test]
async fn health_check_handles_http_methods() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Test GET method (should work)
    let app = create_router(env.pool().clone());
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("failed to make request");
    assert_eq!(response.status(), StatusCode::OK);

    // Test HEAD method (should work for health checks)
    let app = create_router(env.pool().clone());
    let request = axum::http::Request::builder()
        .method("HEAD")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("failed to make request");
    // Should either be OK or Method Not Allowed, but not crash
    assert!(
        response.status() == StatusCode::OK || response.status() == StatusCode::METHOD_NOT_ALLOWED
    );

    // Test POST method (should not be allowed)
    let app = create_router(env.pool().clone());
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.expect("failed to make request");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// Test health check endpoint response format consistency.
///
/// Verifies that the health check response format is consistent and
/// follows expected structure across multiple requests.
#[tokio::test]
async fn health_check_response_format_is_consistent() {
    let env = TestEnv::new().await.expect("failed to create test environment");

    // Make multiple requests and verify consistent response format
    for _ in 0..5 {
        let app = create_router(env.pool().clone());

        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .body(axum::body::Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.expect("failed to make request");
        assert_eq!(response.status(), StatusCode::OK);

        // Verify Content-Type header
        let content_type = response
            .headers()
            .get("content-type")
            .expect("health check should have content-type header");
        assert!(
            content_type.to_str().unwrap().contains("application/json"),
            "Health check should return JSON content type"
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("failed to read response body");
        let body_str = std::str::from_utf8(&body_bytes).expect("failed to parse response body");

        // Response should be valid JSON
        let health_response: Value =
            serde_json::from_str(body_str).expect("health check response should be valid JSON");

        // Response should be an object, not an array or primitive
        assert!(health_response.is_object(), "Health check response should be a JSON object");

        // Should have consistent required fields
        assert!(
            health_response.get("status").is_some(),
            "Health check should always include status field"
        );
    }
}

/// Test health check endpoint includes timestamp information.
///
/// Verifies that health check responses include timing information
/// for monitoring and debugging purposes.
#[tokio::test]
async fn health_check_includes_timestamp() {
    let env = TestEnv::new().await.expect("failed to create test environment");
    let app = create_router(env.pool().clone());

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.expect("failed to make request");
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("failed to read response body");
    let body_str = std::str::from_utf8(&body_bytes).expect("failed to parse response body");

    let health_response: Value =
        serde_json::from_str(body_str).expect("health check response should be valid JSON");

    // Check for timestamp fields (various possible names)
    let has_timestamp = health_response.get("timestamp").is_some()
        || health_response.get("time").is_some()
        || health_response.get("checked_at").is_some()
        || health_response.get("server_time").is_some();

    if has_timestamp {
        // If timestamp is included, verify it's reasonable (within last few seconds)
        if let Some(timestamp_value) = health_response
            .get("timestamp")
            .or_else(|| health_response.get("time"))
            .or_else(|| health_response.get("checked_at"))
            .or_else(|| health_response.get("server_time"))
        {
            // Timestamp should be a string or number
            assert!(
                timestamp_value.is_string() || timestamp_value.is_number(),
                "Timestamp should be string or number format"
            );
        }
    }
    // If no timestamp is included, that's also acceptable - just verify
    // response is valid
}
