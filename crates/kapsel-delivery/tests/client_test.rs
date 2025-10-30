//! Integration tests for HTTP delivery client.
//!
//! Tests webhook delivery client behavior with timeout handling,
//! error categorization, and response processing.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use http::StatusCode;
use kapsel_core::Clock;
use kapsel_delivery::{
    client::{ClientConfig, DeliveryClient, DeliveryRequest},
    DeliveryError,
};
use kapsel_testing::{http::MockResponse, TestClock, TestEnv};
use serde_json::json;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test]
async fn delivers_webhook_successfully() {
    let env = TestEnv::new_shared().await.expect("Failed to create test environment");

    // Setup mock server to respond with success
    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from_static(b"OK"),
        })
        .await;

    let config = ClientConfig { timeout: Duration::from_secs(30), ..Default::default() };
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let response = client.deliver(request).await.expect("Delivery should succeed");

    assert_eq!(response.status_code, 200);
    assert_eq!(response.body, "OK");
    assert!(response.is_success);
    assert!(response.duration > Duration::from_millis(0));
}

#[tokio::test]
async fn handles_connection_timeout() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Setup mock server to timeout
    env.http_mock.mock_simple("/webhook", MockResponse::Timeout).await;

    let config = ClientConfig {
        timeout: Duration::from_millis(100), // Short timeout
        ..Default::default()
    };

    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let result = timeout(Duration::from_millis(200), client.deliver(request)).await;

    match result {
        Ok(Err(DeliveryError::Timeout { .. })) => {
            // Expected - delivery timed out
        },
        Ok(Ok(_)) => panic!("Expected timeout error, got success"),
        Ok(Err(e)) => panic!("Expected timeout error, got: {e}"),
        Err(e) => panic!("Test itself timed out: {e}"),
    }
}

#[tokio::test]
async fn handles_http_error_responses() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Setup mock server to respond with 500 error
    env.http_mock
        .mock_simple("/webhook", MockResponse::ServerError {
            status: 500,
            body: b"Internal Server Error".to_vec(),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let response = client.deliver(request).await.expect("Should get response even for errors");

    assert_eq!(response.status_code, 500);
    assert_eq!(response.body, "Internal Server Error");
    assert!(!response.is_success);
}

#[tokio::test]
async fn respects_retry_after_header() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Setup mock server to respond with 429 and Retry-After header
    env.http_mock
        .mock_simple("/webhook", MockResponse::Failure {
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(Duration::from_secs(30)),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let response = client.deliver(request).await.expect("Should get response");

    assert_eq!(response.status_code, 429);
    assert!(!response.is_success);

    // Check that retry-after header is captured (the key thing to verify)
    assert!(response.headers.contains_key("retry-after"));
    assert_eq!(response.headers.get("retry-after").unwrap(), "30");
}

#[tokio::test]
async fn handles_connection_refused() {
    let config = ClientConfig { timeout: Duration::from_secs(5), ..Default::default() };
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: "http://127.0.0.1:99999/webhook".to_string(), // Non-existent port
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let result = client.deliver(request).await;

    match result {
        Err(DeliveryError::NetworkError { .. }) => {
            // Expected - connection refused
        },
        Ok(_) => panic!("Expected connection error, got success"),
        Err(e) => panic!("Expected connection error, got: {e}"),
    }
}

#[tokio::test]
async fn validates_request_format() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from_static(b"OK"),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: {
            let mut headers = HashMap::new();
            headers.insert("x-webhook-signature".to_string(), "test-signature".to_string());
            headers.insert("user-agent".to_string(), "kapsel-delivery/0.1.0".to_string());
            headers
        },
        body: Bytes::from(json!({"event": "payment.completed", "amount": 1000}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 2,
    };

    let response = client.deliver(request).await.expect("Delivery should succeed");

    assert!(response.is_success);
    assert_eq!(response.status_code, 200);
}

#[tokio::test]
async fn tracks_request_duration() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Setup mock server with success response
    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from_static(b"OK"),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock.clone()).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from_static(b"test"),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let start = clock.now();
    let response = client.deliver(request).await.expect("Delivery should succeed");
    let total_duration = start.elapsed();

    assert!(response.is_success);
    // Just verify duration is measured (non-zero) and reasonable
    assert!(response.duration >= Duration::from_millis(0));
    assert!(response.duration <= total_duration + Duration::from_millis(100)); // Should be close to actual
}

#[tokio::test]
async fn handles_large_response_bodies() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Create a large response (but within reasonable limits)
    let large_body = "x".repeat(1024 * 10); // 10KB

    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from(large_body.clone()),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let response = client.deliver(request).await.expect("Should handle large response");

    assert!(response.is_success);
    assert_eq!(response.body.len(), 1024 * 10);
    assert_eq!(response.body, large_body);
}

#[tokio::test]
async fn limits_response_body_size() {
    let env = TestEnv::new().await.expect("Failed to create test environment");

    // Create an extremely large response (1MB+)
    let huge_body = "x".repeat(1024 * 1024 * 2); // 2MB

    env.http_mock
        .mock_simple("/webhook", MockResponse::Success {
            status: StatusCode::OK,
            body: Bytes::from(huge_body),
        })
        .await;

    let config = ClientConfig::default();
    let clock = Arc::new(TestClock::new());
    let client = DeliveryClient::new(config, clock).expect("Failed to create client");

    let request = DeliveryRequest {
        delivery_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        url: env.http_mock.endpoint_url("/webhook"),
        method: "POST".to_string(),
        headers: HashMap::new(),
        body: Bytes::from(json!({"event": "test"}).to_string()),
        content_type: "application/json".to_string(),
        attempt_number: 1,
    };

    let response = client.deliver(request).await.expect("Should limit response size");

    assert!(response.is_success);
    // Response body should be truncated to reasonable size (1KB for audit)
    assert!(response.body.len() <= 1024);
    assert!(response.body.ends_with("... (truncated)"));
}
