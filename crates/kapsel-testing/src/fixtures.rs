//! Test data builders and fixtures for deterministic testing.
//!
//! Provides builder patterns for webhooks, tenants, and endpoints with
//! configurable properties and sensible defaults for test setup.

use std::collections::HashMap;

use bytes::Bytes;
use chrono::Utc;
use rand::Rng;
use serde_json::{json, Value};
use uuid::Uuid;

/// Builder for test webhook events.
pub struct WebhookBuilder {
    tenant_id: Option<Uuid>,
    endpoint_id: Option<Uuid>,
    source_event_id: Option<String>,
    idempotency_strategy: Option<String>,
    headers: HashMap<String, String>,
    body: Option<Bytes>,
    content_type: Option<String>,
}

impl WebhookBuilder {
    /// Creates a new webhook builder with no defaults.
    pub fn new() -> Self {
        Self {
            tenant_id: None,
            endpoint_id: None,
            source_event_id: None,
            idempotency_strategy: None,
            headers: HashMap::new(),
            body: None,
            content_type: None,
        }
    }

    /// Creates a webhook builder with sensible defaults.
    pub fn with_defaults() -> Self {
        Self {
            tenant_id: Some(Uuid::new_v4()),
            endpoint_id: Some(Uuid::new_v4()),
            source_event_id: Some(format!("evt_{}", Uuid::new_v4().simple())),
            idempotency_strategy: Some("header".to_string()),
            headers: Self::default_headers(),
            body: Some(Bytes::from(r#"{"event": "test.webhook"}"#)),
            content_type: Some("application/json".to_string()),
        }
    }

    fn default_headers() -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("User-Agent".to_string(), "TestClient/1.0".to_string());
        headers.insert("X-Idempotency-Key".to_string(), Uuid::new_v4().to_string());
        headers
    }

    /// Sets the tenant ID for this webhook.
    #[must_use]
    pub fn tenant(mut self, id: Uuid) -> Self {
        self.tenant_id = Some(id);
        self
    }

    /// Sets the endpoint ID for this webhook.
    #[must_use]
    pub fn endpoint(mut self, id: Uuid) -> Self {
        self.endpoint_id = Some(id);
        self
    }

    /// Sets the source event ID for idempotency tracking.
    #[must_use]
    pub fn source_event(mut self, id: impl Into<String>) -> Self {
        self.source_event_id = Some(id.into());
        self
    }

    /// Sets the idempotency strategy (e.g., "source_event_id").
    #[must_use]
    pub fn strategy(mut self, strategy: impl Into<String>) -> Self {
        self.idempotency_strategy = Some(strategy.into());
        self
    }

    /// Adds an HTTP header to the webhook request.
    #[must_use]
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Sets the webhook payload body as raw bytes.
    #[must_use]
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Sets the webhook payload as JSON, automatically setting Content-Type.
    #[must_use]
    pub fn json_body(mut self, value: &Value) -> Self {
        self.body = Some(Bytes::from(value.to_string()));
        self.content_type = Some("application/json".to_string());
        self.headers.insert("Content-Type".to_string(), "application/json".to_string());
        self
    }

    /// Sets the Content-Type header for the webhook payload.
    #[must_use]
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// Builds the webhook event.
    pub fn build(self) -> TestWebhook {
        TestWebhook {
            tenant_id: self.tenant_id.unwrap_or_else(Uuid::new_v4),
            endpoint_id: self.endpoint_id.unwrap_or_else(Uuid::new_v4),
            source_event_id: self
                .source_event_id
                .unwrap_or_else(|| format!("evt_{}", Uuid::new_v4().simple())),
            idempotency_strategy: self.idempotency_strategy.unwrap_or_else(|| "header".to_string()),
            headers: self.headers,
            body: self.body.unwrap_or_else(|| Bytes::from(r#"{"event": "test"}"#)),
            content_type: self.content_type.unwrap_or_else(|| "application/json".to_string()),
        }
    }
}

impl Default for WebhookBuilder {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Test webhook event data.
#[derive(Debug, Clone)]
pub struct TestWebhook {
    /// Tenant that owns this webhook event
    pub tenant_id: Uuid,
    /// Target endpoint for webhook delivery
    pub endpoint_id: Uuid,
    /// External source identifier for idempotency
    pub source_event_id: String,
    /// Strategy used for duplicate detection
    pub idempotency_strategy: String,
    /// HTTP headers to include in webhook request
    pub headers: HashMap<String, String>,
    /// Webhook payload body as raw bytes
    pub body: Bytes,
    /// MIME type of the payload body
    pub content_type: String,
}

/// Builder for test endpoints.
pub struct EndpointBuilder {
    tenant_id: Option<Uuid>,
    name: Option<String>,
    url: Option<String>,
    signing_secret: Option<String>,
    signature_header: Option<String>,
    max_retries: Option<u32>,
    timeout_seconds: Option<u32>,
}

impl EndpointBuilder {
    /// Creates a new endpoint builder with no defaults.
    pub fn new() -> Self {
        Self {
            tenant_id: None,
            name: None,
            url: None,
            signing_secret: None,
            signature_header: None,
            max_retries: None,
            timeout_seconds: None,
        }
    }

    /// Creates an endpoint builder with sensible defaults.
    pub fn with_defaults() -> Self {
        Self {
            tenant_id: Some(Uuid::new_v4()),
            name: Some(format!("test-endpoint-{}", rand::rng().random_range(1000..9999))),
            url: Some("https://example.com/webhook".to_string()),
            signing_secret: Some("test_secret_123".to_string()),
            signature_header: Some("X-Webhook-Signature".to_string()),
            max_retries: Some(3),
            timeout_seconds: Some(30),
        }
    }

    /// Sets the tenant ID for this endpoint.
    #[must_use]
    pub fn tenant(mut self, id: Uuid) -> Self {
        self.tenant_id = Some(id);
        self
    }

    /// Sets a human-readable name for this endpoint.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the target URL for webhook delivery.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets the secret key used for HMAC signature generation.
    #[must_use]
    pub fn signing_secret(mut self, secret: impl Into<String>) -> Self {
        self.signing_secret = Some(secret.into());
        self
    }

    /// Sets the HTTP header name for the HMAC signature.
    #[must_use]
    pub fn signature_header(mut self, header: impl Into<String>) -> Self {
        self.signature_header = Some(header.into());
        self
    }

    /// Sets the maximum number of delivery attempts.
    #[must_use]
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = Some(retries);
        self
    }

    /// Sets the HTTP request timeout in seconds.
    #[must_use]
    pub fn timeout(mut self, seconds: u32) -> Self {
        self.timeout_seconds = Some(seconds);
        self
    }

    /// Builds the endpoint.
    pub fn build(self) -> TestEndpoint {
        TestEndpoint {
            id: Uuid::new_v4(),
            tenant_id: self.tenant_id.unwrap_or_else(Uuid::new_v4),
            name: self.name.unwrap_or_else(|| format!("endpoint-{}", Uuid::new_v4().simple())),
            url: self.url.unwrap_or_else(|| "https://example.com/webhook".to_string()),
            signing_secret: self.signing_secret,
            signature_header: self.signature_header,
            max_retries: self.max_retries.unwrap_or(10),
            timeout_seconds: self.timeout_seconds.unwrap_or(30),
        }
    }
}

impl Default for EndpointBuilder {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Test endpoint data.
#[derive(Debug, Clone)]
pub struct TestEndpoint {
    /// Unique identifier for this endpoint
    pub id: Uuid,
    /// Tenant that owns this endpoint
    pub tenant_id: Uuid,
    /// Human-readable name for the endpoint
    pub name: String,
    /// Target URL for webhook delivery
    pub url: String,
    /// Optional HMAC signing key for request authentication
    pub signing_secret: Option<String>,
    /// HTTP header name for HMAC signature (if signing enabled)
    pub signature_header: Option<String>,
    /// Maximum number of delivery attempts before marking as failed
    pub max_retries: u32,
    /// HTTP request timeout in seconds
    pub timeout_seconds: u32,
}

/// Factory functions for common test scenarios.
pub mod scenarios {
    use chrono::DateTime;

    use super::{
        json, Bytes, EndpointBuilder, TestEndpoint, TestWebhook, Utc, Uuid, WebhookBuilder,
    };

    /// Creates a webhook that will trigger idempotency checks.
    pub fn duplicate_webhook() -> (TestWebhook, TestWebhook) {
        let source_id = format!("evt_{}", Uuid::new_v4().simple());
        let endpoint_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        let first = WebhookBuilder::with_defaults()
            .tenant(tenant_id)
            .endpoint(endpoint_id)
            .source_event(source_id.clone())
            .build();

        let second = WebhookBuilder::with_defaults()
            .tenant(tenant_id)
            .endpoint(endpoint_id)
            .source_event(source_id)
            .body(Bytes::from(r#"{"different": "payload"}"#))
            .build();

        (first, second)
    }

    /// Creates a webhook with invalid signature.
    pub fn unsigned_webhook() -> TestWebhook {
        let mut builder = WebhookBuilder::with_defaults();
        builder.headers.remove("X-Webhook-Signature");
        builder.build()
    }

    /// Creates a webhook that exceeds size limits.
    pub fn oversized_webhook(size_mb: usize) -> TestWebhook {
        let body = Bytes::from(vec![b'x'; size_mb * 1024 * 1024]);
        WebhookBuilder::with_defaults().body(body).content_type("application/octet-stream").build()
    }

    /// Creates a Stripe-style webhook.
    pub fn stripe_webhook() -> TestWebhook {
        WebhookBuilder::new()
            .json_body(&json!({
                "id": "evt_1234567890",
                "object": "event",
                "api_version": "2023-10-16",
                "created": 1_234_567_890,
                "data": {
                    "object": {
                        "id": "ch_1234567890",
                        "object": "charge",
                        "amount": 2000,
                        "currency": "usd",
                        "status": "succeeded"
                    }
                },
                "type": "charge.succeeded"
            }))
            .header("Stripe-Signature", "t=1234567890,v1=abcdef123456,v0=legacy")
            .source_event("evt_1234567890")
            .strategy("source_id")
            .build()
    }

    /// Creates a GitHub webhook.
    pub fn github_webhook() -> TestWebhook {
        WebhookBuilder::new()
            .json_body(&json!({
                "action": "opened",
                "pull_request": {
                    "id": 1,
                    "number": 42,
                    "title": "Test PR",
                    "user": {
                        "login": "testuser"
                    }
                },
                "repository": {
                    "name": "test-repo",
                    "full_name": "testuser/test-repo"
                }
            }))
            .header("X-GitHub-Event", "pull_request")
            .header("X-GitHub-Delivery", Uuid::new_v4().to_string())
            .header("X-Hub-Signature-256", "sha256=abcdef123456")
            .build()
    }

    /// Creates a batch of webhooks for load testing.
    pub fn webhook_batch(count: usize) -> Vec<TestWebhook> {
        let tenant_id = Uuid::new_v4();
        let endpoint_id = Uuid::new_v4();

        (0..count)
            .map(|i| {
                WebhookBuilder::with_defaults()
                    .tenant(tenant_id)
                    .endpoint(endpoint_id)
                    .source_event(format!("batch_evt_{i}"))
                    .json_body({
                        use kapsel_core::Clock;

                        use crate::TestClock;
                        let clock = TestClock::new();
                        let timestamp = DateTime::<Utc>::from(clock.now_system()).to_rfc3339();
                        &json!({
                            "batch_index": i,
                            "timestamp": timestamp,
                            "data": format!("test_data_{}", i)
                        })
                    })
                    .build()
            })
            .collect()
    }

    /// Creates an endpoint that will timeout.
    pub fn slow_endpoint() -> TestEndpoint {
        EndpointBuilder::with_defaults()
            .url("https://slow.example.com/webhook")
            .timeout(1) // 1 second timeout
            .build()
    }

    /// Creates an endpoint with strict retry limits.
    pub fn limited_retry_endpoint() -> TestEndpoint {
        EndpointBuilder::with_defaults().max_retries(1).timeout(5).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_builder_with_defaults() {
        let webhook = WebhookBuilder::with_defaults().build();
        assert!(!webhook.source_event_id.is_empty());
        assert_eq!(webhook.idempotency_strategy, "header");
        assert_eq!(webhook.content_type, "application/json");
    }

    #[test]
    fn webhook_builder_customization() {
        let webhook = WebhookBuilder::new()
            .source_event("custom_event")
            .json_body(&json!({"custom": "data"}))
            .build();

        assert_eq!(webhook.source_event_id, "custom_event");
    }

    #[test]
    fn endpoint_builder_with_defaults() {
        let endpoint = EndpointBuilder::with_defaults().build();
        assert!(endpoint.url.starts_with("https://"));
        assert_eq!(endpoint.max_retries, 3);
        assert_eq!(endpoint.timeout_seconds, 30);
    }

    #[test]
    fn duplicate_webhooks_have_same_id() {
        let (first, second) = scenarios::duplicate_webhook();
        assert_eq!(first.source_event_id, second.source_event_id);
        assert_eq!(first.endpoint_id, second.endpoint_id);
        assert_eq!(first.tenant_id, second.tenant_id);
    }
}
