//! Lightweight test utilities for non-database testing.
//!
//! Provides utilities for tests that don't need database connections,
//! eliminating unnecessary overhead and improving test performance.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use chrono::{DateTime, Utc};
use kapsel_core::Clock;

use crate::{
    http::{MockEndpoint, MockServer},
    time::TestClock,
};

/// Lightweight test utilities without database overhead.
///
/// For tests that only need HTTP mocking, time control, or other
/// utilities without any database operations. Significantly faster
/// setup than full TestEnv.
///
/// Performance characteristics:
/// - Setup time: ~5ms
/// - Memory usage: Minimal
/// - No database connections
pub struct TestUtilities {
    /// Deterministic clock for time-based testing
    pub clock: TestClock,
    /// HTTP mock server for external API simulation
    pub http_mock: Arc<MockServer>,
}

impl TestUtilities {
    /// Creates utilities without database overhead.
    ///
    /// Use this for:
    /// - HTTP client testing
    /// - Cryptographic operations
    /// - Time-based logic
    /// - Business rule validation
    ///
    /// # Example
    ///
    /// ```
    /// # use kapsel_testing::utilities::TestUtilities;
    /// # use std::time::Duration;
    /// # #[tokio::test]
    /// # async fn test_example() {
    /// let utils = TestUtilities::new().await.unwrap();
    ///
    /// // Use HTTP mock
    /// utils.http_mock.mock_endpoint("/webhook").respond_with_status(200).expect(1).mount().await;
    ///
    /// // Control time
    /// utils.advance_time(Duration::from_secs(60));
    /// # }
    /// ```
    pub async fn new() -> Result<Self> {
        let clock = TestClock::new();
        let http_mock = Arc::new(MockServer::start().await);

        Ok(Self { clock, http_mock })
    }

    /// Advance the test clock by the specified duration.
    ///
    /// All time-dependent operations using this clock will see
    /// the advanced time.
    pub fn advance_time(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    /// Get the current time from the test clock.
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        DateTime::<Utc>::from(self.clock.now_system())
    }

    /// Create a new mock endpoint builder for the HTTP server.
    ///
    /// Returns a builder for configuring the endpoint's behavior.
    #[allow(clippy::unused_self)]
    pub fn mock_endpoint_builder(&self, path: &str) -> crate::http::MockEndpoint {
        MockEndpoint::success(path)
    }

    /// Mock an endpoint on the HTTP server.
    pub async fn mock_endpoint(&self, endpoint: MockEndpoint) {
        self.http_mock.mock_endpoint(endpoint).await;
    }

    /// Get the base URL of the mock HTTP server.
    ///
    /// Use this to configure endpoints or clients to point to the mock.
    pub fn mock_url(&self) -> String {
        self.http_mock.url()
    }

    /// Clear all recorded HTTP requests.
    ///
    /// Clears the request history but keeps mock endpoints configured.
    pub async fn clear_requests(&self) {
        self.http_mock.clear_requests().await;
    }

    /// Assert that a specific number of requests were received.
    ///
    /// Call this at the end of tests to ensure expected request count.
    pub async fn assert_request_count(&self, expected: usize) {
        self.http_mock.assert_request_count(expected).await;
    }

    /// Create a clock Arc for passing to production code.
    ///
    /// Many production APIs expect `Arc<dyn Clock>`.
    pub fn clock_arc(&self) -> Arc<dyn kapsel_core::Clock> {
        Arc::new(self.clock.clone())
    }
}

/// Extended utilities for specific test scenarios.
impl TestUtilities {
    /// Create utilities with pre-configured mock endpoints.
    ///
    /// Useful for tests that need standard mock responses.
    pub async fn with_default_mocks() -> Result<Self> {
        let utils = Self::new().await?;

        // Configure common mock endpoints
        let health_endpoint = MockEndpoint::success("/health");
        utils.mock_endpoint(health_endpoint).await;

        Ok(utils)
    }

    /// Simulate time passing with periodic ticks.
    ///
    /// Advances time in small increments, useful for testing
    /// time-based state machines or timeouts.
    pub async fn tick_time(&self, interval: Duration, ticks: usize) {
        for _ in 0..ticks {
            self.advance_time(interval);
            // Yield to allow async tasks to process time change
            tokio::task::yield_now().await;
        }
    }

    /// Wait for a condition with timeout using test clock.
    ///
    /// Advances test time while checking a condition, simulating
    /// waiting without actual delays.
    pub async fn wait_for<F>(&self, mut condition: F, timeout: Duration) -> Result<()>
    where
        F: FnMut() -> bool,
    {
        let start = self.now();
        let interval = Duration::from_millis(100);
        let timeout_chrono = chrono::Duration::from_std(timeout)
            .map_err(|e| anyhow::anyhow!("invalid timeout duration: {e}"))?;

        while !condition() {
            if self.now() - start > timeout_chrono {
                anyhow::bail!("timeout waiting for condition");
            }
            self.advance_time(interval);
            tokio::task::yield_now().await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn utilities_create_quickly() {
        let start = std::time::Instant::now();
        let utils = TestUtilities::new().await.expect("create utilities");
        let elapsed = start.elapsed();

        // Should be very fast without database
        assert!(elapsed < Duration::from_millis(100));

        // Utilities should be functional
        let initial_time = utils.now();
        utils.advance_time(Duration::from_secs(60));
        assert_eq!(utils.now() - initial_time, chrono::Duration::seconds(60));
    }

    #[tokio::test]
    async fn http_mock_works_without_database() {
        let utils = TestUtilities::new().await.expect("create utilities");

        let endpoint = utils.mock_endpoint_builder("/test").with_body(
            serde_json::to_vec(&serde_json::json!({
                "status": "ok"
            }))
            .unwrap(),
        );
        utils.mock_endpoint(endpoint).await;

        // Create HTTP client
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/test", utils.mock_url()))
            .json(&serde_json::json!({"request": "data"}))
            .send()
            .await
            .expect("send request");

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.expect("parse json");
        assert_eq!(body["status"], "ok");

        // Mock server successfully responded with expected data
    }

    #[tokio::test]
    async fn time_control_is_deterministic() {
        let utils = TestUtilities::new().await.expect("create utilities");

        let t1 = utils.now();
        utils.advance_time(Duration::from_secs(1));
        let t2 = utils.now();
        utils.advance_time(Duration::from_secs(2));
        let t3 = utils.now();

        assert_eq!(t2 - t1, chrono::Duration::seconds(1));
        assert_eq!(t3 - t2, chrono::Duration::seconds(2));
        assert_eq!(t3 - t1, chrono::Duration::seconds(3));
    }

    #[tokio::test]
    async fn tick_time_advances_incrementally() {
        let utils = TestUtilities::new().await.expect("create utilities");

        let start = utils.now();
        utils.tick_time(Duration::from_secs(1), 5).await;
        let end = utils.now();

        assert_eq!(end - start, chrono::Duration::seconds(5));
    }

    #[tokio::test]
    async fn wait_for_condition_succeeds() {
        let utils = TestUtilities::new().await.expect("create utilities");

        let start = utils.now();
        let mut counter = 0;

        utils
            .wait_for(
                || {
                    counter += 1;
                    counter >= 5
                },
                Duration::from_secs(10),
            )
            .await
            .expect("wait for condition");

        // Should have advanced time while waiting
        assert!(utils.now() > start);
    }

    #[tokio::test]
    async fn wait_for_condition_times_out() {
        let utils = TestUtilities::new().await.expect("create utilities");

        let result = utils.wait_for(|| false, Duration::from_secs(1)).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }
}
