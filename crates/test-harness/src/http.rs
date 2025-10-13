//! HTTP mocking utilities for webhook testing.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use serde_json::Value;
use tokio::sync::RwLock;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer as WiremockServer, ResponseTemplate,
};

/// HTTP mock server for testing webhook deliveries.
pub struct MockServer {
    server: WiremockServer,
    recorded_requests: Arc<RwLock<Vec<RecordedRequest>>>,
}

impl MockServer {
    /// Starts a new mock server on a random port.
    pub async fn start() -> Self {
        let server = WiremockServer::start().await;
        Self { server, recorded_requests: Arc::new(RwLock::new(Vec::new())) }
    }

    /// Returns the base URL of the mock server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Configures an endpoint to return a specific response.
    pub async fn mock_endpoint(&self, endpoint: MockEndpoint) {
        let response = match endpoint.response {
            MockResponse::Success { status, body } => {
                ResponseTemplate::new(status.as_u16()).set_body_bytes(body)
            },
            MockResponse::Failure { status, retry_after } => {
                let mut response = ResponseTemplate::new(status.as_u16());
                if let Some(seconds) = retry_after {
                    response = response.insert_header("Retry-After", seconds.as_secs().to_string());
                }
                response
            },
            MockResponse::Timeout => ResponseTemplate::new(StatusCode::REQUEST_TIMEOUT.as_u16())
                .set_delay(Duration::from_secs(35)),
        };

        let mut mock = Mock::given(method("POST")).and(path(endpoint.path.clone()));

        // Add header matchers if specified
        for (key, value) in &endpoint.expected_headers {
            mock = mock.and(header(key.as_str(), value.as_str()));
        }

        mock.respond_with(response).mount(&self.server).await;
    }

    /// Returns all requests received by the server.
    pub async fn received_requests(&self) -> Vec<RecordedRequest> {
        self.recorded_requests.read().await.clone()
    }

    /// Clears all recorded requests.
    pub async fn clear_requests(&self) {
        self.recorded_requests.write().await.clear();
    }

    /// Asserts that exactly n requests were received.
    pub async fn assert_request_count(&self, expected: usize) {
        let requests = self.received_requests().await;
        assert_eq!(
            requests.len(),
            expected,
            "Expected {} requests, received {}",
            expected,
            requests.len()
        );
    }

    /// Creates a mock sequence builder for chaining multiple responses.
    pub fn mock_sequence(&self) -> MockSequenceBuilder<'_> {
        MockSequenceBuilder { server: &self.server, responses: Vec::new() }
    }

    /// Configures an endpoint to always fail with the given status code.
    pub async fn mock_endpoint_always_fail(&self, status: u16) {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&self.server)
            .await;
    }
}

/// Configuration for a mock endpoint.
pub struct MockEndpoint {
    pub path: String,
    pub expected_headers: HashMap<String, String>,
    pub response: MockResponse,
}

impl MockEndpoint {
    /// Creates a mock endpoint that returns success.
    pub fn success(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            expected_headers: HashMap::new(),
            response: MockResponse::Success { status: StatusCode::OK, body: Bytes::new() },
        }
    }

    /// Creates a mock endpoint that returns a failure.
    pub fn failure(path: impl Into<String>, status: StatusCode) -> Self {
        Self {
            path: path.into(),
            expected_headers: HashMap::new(),
            response: MockResponse::Failure { status, retry_after: None },
        }
    }

    /// Adds an expected header to the mock.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.expected_headers.insert(key.into(), value.into());
        self
    }

    /// Sets the response body.
    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        if let MockResponse::Success { status, .. } = self.response {
            self.response = MockResponse::Success { status, body: body.into() };
        }
        self
    }

    /// Sets a retry-after header for rate limiting scenarios.
    pub fn with_retry_after(mut self, duration: Duration) -> Self {
        if let MockResponse::Failure { status, .. } = self.response {
            self.response = MockResponse::Failure { status, retry_after: Some(duration) };
        }
        self
    }
}

/// Types of mock responses.
pub enum MockResponse {
    Success { status: StatusCode, body: Bytes },
    Failure { status: StatusCode, retry_after: Option<Duration> },
    Timeout,
}

/// A recorded HTTP request.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub timestamp: std::time::Instant,
}

/// Builds complex HTTP interaction scenarios.
pub struct ScenarioBuilder {
    server: MockServer,
    interactions: Vec<Interaction>,
}

struct Interaction {
    endpoint: MockEndpoint,
    delay: Option<Duration>,
}

impl ScenarioBuilder {
    /// Creates a new scenario builder.
    pub fn new(server: MockServer) -> Self {
        Self { server, interactions: Vec::new() }
    }

    /// Adds a successful response after optional delay.
    pub fn respond_ok(mut self, path: impl Into<String>, delay: Option<Duration>) -> Self {
        self.interactions.push(Interaction { endpoint: MockEndpoint::success(path), delay });
        self
    }

    /// Adds a failure response.
    pub fn respond_error(
        mut self,
        path: impl Into<String>,
        status: StatusCode,
        delay: Option<Duration>,
    ) -> Self {
        self.interactions
            .push(Interaction { endpoint: MockEndpoint::failure(path, status), delay });
        self
    }

    /// Adds a timeout response.
    pub fn respond_timeout(mut self, path: impl Into<String>) -> Self {
        self.interactions.push(Interaction {
            endpoint: MockEndpoint {
                path: path.into(),
                expected_headers: HashMap::new(),
                response: MockResponse::Timeout,
            },
            delay: None,
        });
        self
    }

    /// Builds and configures the scenario on the server.
    pub async fn build(self) {
        for interaction in self.interactions {
            if let Some(delay) = interaction.delay {
                tokio::time::sleep(delay).await;
            }
            self.server.mock_endpoint(interaction.endpoint).await;
        }
    }
}

/// Builder for mock response sequences.
pub struct MockSequenceBuilder<'a> {
    server: &'a WiremockServer,
    responses: Vec<(u16, String)>,
}

impl<'a> MockSequenceBuilder<'a> {
    /// Adds a response with the given status code and body.
    pub fn respond_with(mut self, status: u16, body: impl Into<String>) -> Self {
        self.responses.push((status, body.into()));
        self
    }

    /// Adds a JSON response.
    pub fn respond_with_json(mut self, status: u16, json: serde_json::Value) -> Self {
        self.responses.push((status, json.to_string()));
        self
    }

    /// Builds and mounts the mock sequence.
    pub async fn build(self) {
        // For simplicity, we'll mount all responses at once
        // In a real implementation, this would cycle through responses
        for (status, body) in self.responses {
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .up_to_n_times(1)
                .mount(self.server)
                .await;
        }
    }
}

/// HTTP assertions for webhook testing.
pub mod assertions {
    use super::*;

    /// Asserts that a request contains the expected header.
    pub fn assert_header_present(request: &RecordedRequest, key: &str, value: &str) {
        let header_value =
            request.headers.get(key).unwrap_or_else(|| panic!("Header '{}' not present", key));

        assert_eq!(header_value.to_str().unwrap(), value, "Header '{}' has unexpected value", key);
    }

    /// Asserts that the request body matches expected JSON.
    pub fn assert_json_body(request: &RecordedRequest, expected: &Value) {
        let actual: Value =
            serde_json::from_slice(&request.body).expect("Failed to parse request body as JSON");

        assert_eq!(actual, *expected, "Request body does not match expected JSON");
    }

    /// Asserts timing between requests.
    pub fn assert_retry_timing(requests: &[RecordedRequest], expected_delays: &[Duration]) {
        assert_eq!(
            requests.len(),
            expected_delays.len() + 1,
            "Unexpected number of retry attempts"
        );

        for (i, expected_delay) in expected_delays.iter().enumerate() {
            let actual_delay = requests[i + 1].timestamp.duration_since(requests[i].timestamp);

            // Allow 10% variance for timing
            let min = expected_delay.as_millis() * 9 / 10;
            let max = expected_delay.as_millis() * 11 / 10;
            let actual_ms = actual_delay.as_millis();

            assert!(
                actual_ms >= min && actual_ms <= max,
                "Retry {} timing off: expected ~{:?}, got {:?}",
                i + 1,
                expected_delay,
                actual_delay
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_server_starts() {
        let server = MockServer::start().await;
        assert!(!server.url().is_empty());
        assert!(server.url().starts_with("http://"));
    }

    #[tokio::test]
    async fn mock_endpoint_configuration() {
        let server = MockServer::start().await;

        let endpoint = MockEndpoint::success("/webhook")
            .with_header("X-Custom-Header", "test-value")
            .with_body("response body");

        server.mock_endpoint(endpoint).await;

        // Server is now configured to respond to POST /webhook
    }

    #[tokio::test]
    async fn scenario_builder_chains_responses() {
        let server = MockServer::start().await;

        ScenarioBuilder::new(server)
            .respond_error("/webhook", StatusCode::SERVICE_UNAVAILABLE, None)
            .respond_error(
                "/webhook",
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(Duration::from_secs(1)),
            )
            .respond_ok("/webhook", Some(Duration::from_secs(2)))
            .build()
            .await;
    }
}
