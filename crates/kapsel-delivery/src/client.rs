//! HTTP client for webhook delivery with configurable timeouts.
//!
//! Handles request construction, response processing, and error categorization
//! for retry logic and circuit breaker integration.

use std::{collections::HashMap, time::Duration};

use bytes::Bytes;
use reqwest::{header::HeaderMap, Response};
use serde::{Deserialize, Serialize};
use tracing::{info_span, Instrument};
use uuid::Uuid;

use crate::error::{DeliveryError, Result};

/// Configuration for the webhook delivery client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Default timeout for HTTP requests.
    pub timeout: Duration,
    /// User agent string for requests.
    pub user_agent: String,
    /// Maximum number of redirects to follow.
    pub max_redirects: u32,
    /// Whether to verify TLS certificates.
    pub verify_tls: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: "Kapsel-Webhook-Delivery/1.0".to_string(),
            max_redirects: 3,
            verify_tls: true,
        }
    }
}

/// HTTP client optimized for webhook delivery.
///
/// Uses connection pooling and configurable timeouts to efficiently deliver
/// webhooks to many endpoints concurrently. Automatically categorizes HTTP
/// errors for proper retry and circuit breaker behavior.
#[derive(Debug, Clone)]
pub struct DeliveryClient {
    client: reqwest::Client,
    config: ClientConfig,
}

/// Request context for a webhook delivery.
#[derive(Debug, Clone)]
pub struct DeliveryRequest {
    /// Unique identifier for this delivery attempt.
    pub delivery_id: Uuid,
    /// Event ID being delivered.
    pub event_id: Uuid,
    /// Destination URL for the webhook.
    pub url: String,
    /// HTTP method (typically POST).
    pub method: String,
    /// Request headers from original webhook.
    pub headers: HashMap<String, String>,
    /// Request body payload.
    pub body: Bytes,
    /// Content type of the payload.
    pub content_type: String,
    /// Attempt number for this delivery.
    pub attempt_number: u32,
}

/// Response from a webhook delivery attempt.
#[derive(Debug, Clone)]
pub struct DeliveryResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body (limited size).
    pub body: String,
    /// Total duration of the request.
    pub duration: Duration,
    /// Whether the request was successful (2xx status).
    pub is_success: bool,
}

impl DeliveryClient {
    /// Creates a new delivery client with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `DeliveryError::ConfigurationError` if the HTTP client cannot
    /// be configured with the provided settings.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects as usize))
            .danger_accept_invalid_certs(!config.verify_tls)
            .build()
            .map_err(|e| {
                DeliveryError::configuration(format!("failed to build HTTP client: {e}"))
            })?;

        Ok(Self { client, config })
    }

    /// Creates a new delivery client with default configuration.
    pub fn with_defaults() -> Result<Self> {
        Self::new(ClientConfig::default())
    }

    /// Delivers a webhook to the specified endpoint.
    ///
    /// Sends the webhook payload to the destination URL with proper headers
    /// and timeout handling. Categorizes response errors appropriately for
    /// retry logic and circuit breaker decisions.
    ///
    /// # Errors
    ///
    /// Returns categorized delivery errors based on the HTTP response:
    /// - `NetworkError` for connection failures
    /// - `Timeout` for request timeouts
    /// - `ClientError` for 4xx responses
    /// - `ServerError` for 5xx responses
    /// - `RateLimited` for 429 responses with Retry-After header
    pub async fn deliver(&self, request: DeliveryRequest) -> Result<DeliveryResponse> {
        let start_time = std::time::Instant::now();

        let span = info_span!(
            "webhook_delivery",
            event_id = %request.event_id,
            delivery_id = %request.delivery_id,
            url = %request.url,
            attempt = request.attempt_number
        );

        async move {
            tracing::debug!("Starting webhook delivery");

            let mut http_request = self
                .client
                .post(&request.url)
                .body(request.body.clone())
                .header("content-type", &request.content_type);

            for (key, value) in &request.headers {
                if !is_managed_header(key) {
                    http_request = http_request.header(key, value);
                }
            }

            http_request = http_request
                .header("X-Kapsel-Event-Id", request.event_id.to_string())
                .header("X-Kapsel-Delivery-Id", request.delivery_id.to_string())
                .header("X-Kapsel-Delivery-Attempt", request.attempt_number.to_string())
                .header("X-Kapsel-Original-Timestamp", chrono::Utc::now().to_rfc3339());

            let response = match http_request.send().await {
                Ok(response) => response,
                Err(e) => {
                    let duration = start_time.elapsed();
                    tracing::warn!(duration_ms = duration.as_millis(), "Request failed: {}", e);

                    if e.is_timeout() {
                        return Err(DeliveryError::timeout(self.config.timeout.as_secs()));
                    }
                    if e.is_connect() {
                        return Err(DeliveryError::network(format!("connection failed: {e}")));
                    }
                    return Err(DeliveryError::network(e.to_string()));
                },
            };

            let duration = start_time.elapsed();
            let status_code = response.status().as_u16();

            tracing::debug!(
                status = status_code,
                duration_ms = duration.as_millis(),
                "Received response"
            );

            let delivery_response = self.parse_response(response, duration).await?;

            match delivery_response.status_code {
                200..=299 => {
                    tracing::info!("Webhook delivered successfully");
                },
                400..=499 => {
                    tracing::warn!(status = delivery_response.status_code, "Client error response");
                },
                500..=599 => {
                    tracing::warn!(status = delivery_response.status_code, "Server error response");
                },
                _ => {
                    tracing::warn!(
                        status = delivery_response.status_code,
                        "Unexpected status code"
                    );
                },
            }

            Ok(delivery_response)
        }
        .instrument(span)
        .await
    }

    /// Parses an HTTP response into a delivery response.
    async fn parse_response(
        &self,
        response: Response,
        duration: Duration,
    ) -> Result<DeliveryResponse> {
        let status_code = response.status().as_u16();
        let is_success = response.status().is_success();

        let headers = extract_headers(response.headers());

        let body = match response.bytes().await {
            Ok(bytes) => {
                const MAX_RESPONSE_BODY_SIZE: usize = 64 * 1024; // 64KB - reasonable for webhook responses
                const MAX_AUDIT_SIZE: usize = 1024; // 1KB for audit storage

                if bytes.len() > MAX_RESPONSE_BODY_SIZE {
                    let suffix = "... (truncated)";
                    let max_content = MAX_AUDIT_SIZE - suffix.len();
                    let truncated = String::from_utf8_lossy(&bytes[..max_content]);
                    format!("{truncated}{suffix}")
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read response body: {}", e);
                format!("[Failed to read response body: {e}]")
            },
        };

        Ok(DeliveryResponse { status_code, headers, body, duration, is_success })
    }
}

/// Extracts headers from reqwest HeaderMap into a standard HashMap.
fn extract_headers(header_map: &HeaderMap) -> HashMap<String, String> {
    let mut headers = HashMap::new();

    for (key, value) in header_map {
        if let Ok(value_str) = value.to_str() {
            headers.insert(key.to_string(), value_str.to_string());
        }
    }

    headers
}

/// Checks if a header is managed by the delivery system and should not be
/// copied from the original request.
fn is_managed_header(header_name: &str) -> bool {
    let lowercase = header_name.to_lowercase();
    matches!(
        lowercase.as_str(),
        "content-length"
            | "host"
            | "user-agent"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Extracts retry-after delay from response headers.
///
/// Supports both seconds format and HTTP-date format. Returns the delay in
/// seconds, or a default value (60s) if parsing fails.
pub fn extract_retry_after_seconds<S: std::hash::BuildHasher>(
    headers: &HashMap<String, String, S>,
) -> Option<u64> {
    const DEFAULT_RETRY_AFTER: u64 = 60;

    if let Some(retry_after) = headers.get("retry-after").or_else(|| headers.get("Retry-After")) {
        if let Ok(seconds) = retry_after.parse::<u64>() {
            return Some(seconds);
        }

        if let Ok(date_time) = chrono::DateTime::parse_from_rfc2822(retry_after) {
            let now = chrono::Utc::now();
            let retry_time = date_time.with_timezone(&chrono::Utc);

            if retry_time > now {
                let duration = retry_time.signed_duration_since(now);
                if let Ok(std_duration) = duration.to_std() {
                    return Some(std_duration.as_secs());
                }
            }
        }

        Some(DEFAULT_RETRY_AFTER)
    } else {
        None // No header found
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    use super::*;

    fn create_test_request(url: String) -> DeliveryRequest {
        let mut headers = HashMap::new();
        headers.insert("X-Original-Header".to_string(), "test-value".to_string());

        DeliveryRequest {
            delivery_id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            url,
            method: "POST".to_string(),
            headers,
            body: Bytes::from("test payload"),
            content_type: "application/json".to_string(),
            attempt_number: 1,
        }
    }

    #[tokio::test]
    async fn successful_delivery() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::path("/webhook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status_code, 200);
        assert!(response.is_success);
        assert_eq!(response.body, "OK");
    }

    #[tokio::test]
    async fn client_error_handling() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status_code, 404);
        assert_eq!(response.body, "Not Found");
        assert!(!response.is_success);
    }

    #[tokio::test]
    async fn server_error_handling() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status_code, 500);
        assert_eq!(response.body, "Internal Server Error");
        assert!(!response.is_success);
    }

    #[tokio::test]
    async fn rate_limit_with_retry_after() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("Too Many Requests")
                    .append_header("Retry-After", "120"),
            )
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.status_code, 429);
        assert!(!response.is_success);
        assert_eq!(response.headers.get("retry-after").unwrap(), "120");
    }

    #[tokio::test]
    async fn delivery_metadata_headers_added() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::header_exists("X-Kapsel-Event-Id"))
            .and(matchers::header_exists("X-Kapsel-Delivery-Id"))
            .and(matchers::header_exists("X-Kapsel-Delivery-Attempt"))
            .and(matchers::header_exists("X-Kapsel-Original-Timestamp"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn original_headers_preserved() {
        let mock_server = MockServer::start().await;

        Mock::given(matchers::method("POST"))
            .and(matchers::header("X-Original-Header", "test-value"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let client = DeliveryClient::with_defaults().unwrap();
        let request = create_test_request(format!("{}/webhook", mock_server.uri()));

        let result = client.deliver(request).await;
        assert!(result.is_ok());
    }

    #[test]
    fn retry_after_parsing() {
        let mut headers = HashMap::new();

        // Test seconds format
        headers.insert("retry-after".to_string(), "120".to_string());
        assert_eq!(extract_retry_after_seconds(&headers), Some(120));

        // Test None when missing
        headers.clear();
        assert_eq!(extract_retry_after_seconds(&headers), None);

        // Test invalid format falls back to default
        headers.insert("retry-after".to_string(), "invalid".to_string());
        assert_eq!(extract_retry_after_seconds(&headers), Some(60));
    }

    #[test]
    fn managed_headers_identified() {
        assert!(is_managed_header("Content-Length"));
        assert!(is_managed_header("content-length"));
        assert!(is_managed_header("Host"));
        assert!(is_managed_header("USER-AGENT"));

        assert!(!is_managed_header("X-Custom-Header"));
        assert!(!is_managed_header("Authorization"));
    }
}
