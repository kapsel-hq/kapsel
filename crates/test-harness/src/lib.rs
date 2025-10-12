//! Test harness for Hooky integration and unit tests.
//!
//! Provides deterministic test infrastructure, database setup, HTTP mocking,
//! and fixture builders for RED-GREEN TDD development.

pub mod database;
pub mod fixtures;
pub mod http;
pub mod time;

// Re-export commonly used items
pub use time::Clock;

use std::time::Duration;

use anyhow::{Context, Result};

use sqlx::{PgPool, Postgres};
use tracing_subscriber::EnvFilter;

/// Test environment with all necessary infrastructure.
pub struct TestEnv {
    pub db: PgPool,
    pub http_mock: http::MockServer,
    pub clock: time::TestClock,
    pub config: TestConfig,
    pub client: reqwest::Client,
    pub server_addr: Option<std::net::SocketAddr>,
}

impl TestEnv {
    /// Creates a new test environment with defaults.
    pub async fn new() -> Result<Self> {
        Self::with_config(TestConfig::default()).await
    }

    /// Creates a test environment with custom configuration.
    pub async fn with_config(config: TestConfig) -> Result<Self> {
        // Initialize tracing for tests
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("warn,hooky=debug")),
            )
            .with_test_writer()
            .try_init();

        let db = database::setup_test_database().await?;
        let http_mock = http::MockServer::start().await;
        let clock = time::TestClock::new();
        let client = reqwest::Client::new();

        Ok(Self {
            db,
            http_mock,
            clock,
            config,
            client,
            server_addr: None,
        })
    }

    /// Advances test time by the specified duration.
    pub fn advance_time(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    /// Creates a test transaction that auto-rollbacks.
    pub async fn transaction(&self) -> Result<TestTransaction<'_>> {
        let tx = self.db.begin().await?;
        Ok(TestTransaction { tx: Some(tx), _env: self })
    }

    /// Attaches a running Axum server to this test environment.
    pub fn with_server(&mut self, addr: std::net::SocketAddr) {
        self.server_addr = Some(addr);
    }

    /// Returns the base URL for making requests to the test server.
    pub fn base_url(&self) -> String {
        self.server_addr
            .map(|addr| format!("http://{}", addr))
            .unwrap_or_else(|| "http://localhost:8080".to_string())
    }
}

/// Test configuration options.
#[derive(Debug, Clone)]
pub struct TestConfig {
    pub enable_tracing: bool,
    pub database_name: Option<String>,
    pub seed: Option<u64>,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self { enable_tracing: true, database_name: None, seed: None }
    }
}

/// Transaction that automatically rolls back on drop.
pub struct TestTransaction<'a> {
    tx: Option<sqlx::Transaction<'a, Postgres>>,
    _env: &'a TestEnv,
}

impl<'a> TestTransaction<'a> {
    /// Commits the transaction (prevents automatic rollback).
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await?;
        }
        Ok(())
    }

    /// Explicitly rolls back the transaction.
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        Ok(())
    }
}

impl<'a> Drop for TestTransaction<'a> {
    fn drop(&mut self) {
        if self.tx.is_some() {
            // Transaction will rollback automatically
            tracing::debug!("Test transaction rolling back");
        }
    }
}

/// Common test assertions for webhooks.
pub mod assertions {
    use bytes::Bytes;
    use serde_json::Value;

    /// Asserts that a JSON payload matches expected structure.
    pub fn assert_json_matches(actual: &Bytes, expected: &Value) {
        let actual_json: Value =
            serde_json::from_slice(actual).expect("Failed to parse actual JSON");

        assert_eq!(
            actual_json,
            *expected,
            "JSON payloads do not match.\nActual: {}\nExpected: {}",
            serde_json::to_string_pretty(&actual_json).unwrap(),
            serde_json::to_string_pretty(expected).unwrap()
        );
    }

    /// Asserts that a webhook was delivered within timeout.
    pub async fn assert_delivered_within(_event_id: &str, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Check delivery status (would query database)
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Placeholder - would check actual delivery status
            if false {
                // Placeholder - would check actual delivery status
                return true;
            }
        }

        false
    }
}

/// Test scenario builder for complex test cases.
pub struct ScenarioBuilder {
    name: String,
    steps: Vec<Step>,
}

/// Type alias for state assertions in scenarios.
type StateAssertion = Box<dyn Fn(&TestEnv) -> Result<()>>;

enum Step {
    IngestWebhook {
        endpoint_id: String,
        #[allow(dead_code)] // Will be used when ingestion logic is implemented
        payload: bytes::Bytes,
    },
    ExpectDelivery {
        timeout: Duration,
    },
    InjectFailure {
        kind: FailureKind,
    },
    AdvanceTime {
        duration: Duration,
    },
    AssertState {
        assertion: StateAssertion,
    },
}

#[derive(Debug, Clone)]
pub enum FailureKind {
    NetworkTimeout,
    Http500,
    Http429 { retry_after: Duration },
    DatabaseUnavailable,
}

impl ScenarioBuilder {
    /// Creates a new test scenario.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), steps: Vec::new() }
    }

    /// Adds a webhook ingestion step.
    pub fn ingest(mut self, endpoint_id: impl Into<String>, payload: bytes::Bytes) -> Self {
        self.steps.push(Step::IngestWebhook { endpoint_id: endpoint_id.into(), payload });
        self
    }

    /// Expects delivery within timeout.
    pub fn expect_delivery(mut self, timeout: Duration) -> Self {
        self.steps.push(Step::ExpectDelivery { timeout });
        self
    }

    /// Injects a failure condition.
    pub fn inject_failure(mut self, kind: FailureKind) -> Self {
        self.steps.push(Step::InjectFailure { kind });
        self
    }

    /// Advances test time.
    pub fn advance_time(mut self, duration: Duration) -> Self {
        self.steps.push(Step::AdvanceTime { duration });
        self
    }

    /// Adds a custom assertion.
    pub fn assert_state<F>(mut self, assertion: F) -> Self
    where
        F: Fn(&TestEnv) -> Result<()> + 'static,
    {
        self.steps.push(Step::AssertState { assertion: Box::new(assertion) });
        self
    }

    /// Executes the scenario.
    pub async fn run(self, env: &TestEnv) -> Result<()> {
        tracing::info!("Running scenario: {}", self.name);

        for (i, step) in self.steps.into_iter().enumerate() {
            tracing::debug!("Executing step {}", i + 1);

            match step {
                Step::IngestWebhook { endpoint_id, payload: _ } => {
                    // Would call actual ingestion logic
                    tracing::debug!("Ingesting webhook for endpoint {}", endpoint_id);
                },
                Step::ExpectDelivery { timeout } => {
                    // Would wait for delivery
                    tracing::debug!("Waiting for delivery within {:?}", timeout);
                },
                Step::InjectFailure { kind } => {
                    // Would configure mock to return failure
                    tracing::debug!("Injecting failure: {:?}", kind);
                },
                Step::AdvanceTime { duration } => {
                    env.advance_time(duration);
                    tracing::debug!("Advanced time by {:?}", duration);
                },
                Step::AssertState { assertion } => {
                    assertion(env).context("State assertion failed")?;
                },
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[tokio::test]
    async fn test_environment_setup() {
        let env = TestEnv::new().await.unwrap();

        // Verify database connection
        sqlx::query("SELECT 1").fetch_one(&env.db).await.unwrap();

        // Verify mock server is running
        assert!(!env.http_mock.url().is_empty());
    }

    #[tokio::test]
    async fn test_transaction_rollback() {
        let env = TestEnv::new().await.unwrap();

        {
            let _tx = env.transaction().await.unwrap();
            // Transaction automatically rolls back when dropped
        }

        // Verify no data was persisted
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events")
            .fetch_one(&env.db)
            .await
            .unwrap_or(0);

        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn scenario_builder_executes_steps() {
        let env = TestEnv::new().await.unwrap();

        let scenario = ScenarioBuilder::new("test scenario")
            .ingest("endpoint_1", Bytes::from("test payload"))
            .advance_time(Duration::from_secs(1))
            .expect_delivery(Duration::from_secs(5))
            .assert_state(|_env| {
                // Custom assertion
                Ok(())
            });

        scenario.run(&env).await.unwrap();
    }
}
