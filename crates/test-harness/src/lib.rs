//! Test harness for Kapsel integration and unit tests.
//!
//! Provides deterministic test infrastructure with transaction-based database
//! isolation, HTTP mocking, and fixture builders for reliable testing.

pub mod database;
pub mod fixtures;
pub mod http;
pub mod invariants;
pub mod time;

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use database::TestDatabase;
pub use invariants::{assertions as invariant_assertions, strategies, Invariants};
use kapsel_core::models::{EndpointId, EventId, TenantId};
use sqlx::{PgPool, Postgres, Row};
pub use time::Clock;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::fixtures::TestWebhook;

/// Type alias for invariant check functions to reduce complexity
type InvariantCheckFn = Box<dyn Fn(&mut TestEnv) -> Result<()>>;

/// Type alias for assertion functions to reduce complexity
type AssertionFn = Box<dyn Fn(&mut TestEnv) -> Result<()>>;

/// Test environment with transaction-based database isolation.
///
/// Each test gets its own TestEnv with an isolated database transaction
/// that automatically rolls back when dropped. This ensures perfect
/// isolation between tests with minimal overhead.
pub struct TestEnv {
    /// Database transaction for this test - automatically rolled back on drop
    pub tx: Option<sqlx::Transaction<'static, Postgres>>,
    /// HTTP mock server for external API simulation
    pub http_mock: http::MockServer,
    /// Deterministic clock for time-based testing
    pub clock: time::TestClock,
    /// Shared database instance (Arc ensures proper cleanup)
    shared_db: std::sync::Arc<database::SharedDatabase>,
}

impl TestEnv {
    /// Create a new test environment with transaction isolation.
    ///
    /// This sets up:
    /// - A database transaction that auto-rollbacks
    /// - An HTTP mock server for external calls
    /// - A deterministic test clock
    pub async fn new() -> Result<Self> {
        // Initialize tracing once per process
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("warn,kapsel=debug")),
            )
            .with_test_writer()
            .try_init();

        // Create database and begin transaction
        let shared_db = database::get_shared_database().await?;
        let db = TestDatabase::new().await?;
        let tx = db.begin_transaction().await?;

        // Create HTTP mock server
        let http_mock = http::MockServer::start().await;

        // Create deterministic clock
        let clock = time::TestClock::new();

        Ok(Self { tx: Some(tx), http_mock, clock, shared_db })
    }

    /// Get a mutable reference to the database executor.
    ///
    /// All database operations in tests should go through this
    /// to ensure they're part of the transaction and will be
    /// rolled back after the test.
    pub fn db(&mut self) -> &mut sqlx::Transaction<'static, Postgres> {
        self.tx.as_mut().expect("transaction should be active")
    }

    /// Create a new connection pool for components that manage their own
    /// connections.
    ///
    /// This is useful for testing delivery workers and other components
    /// that need their own database connections. Note that these connections
    /// won't see uncommitted data from the test transaction unless you
    /// explicitly commit it first.
    pub fn create_pool(&self) -> PgPool {
        self.shared_db.pool().clone()
    }

    /// Commit the test transaction to make data visible to other connections.
    ///
    /// This consumes the TestEnv since the transaction can't be used after
    /// commit. Useful for tests that need to verify behavior with multiple
    /// database connections.
    pub async fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await.context("failed to commit transaction")?;
        }
        Ok(())
    }

    /// Advance the test clock by the specified duration.
    pub fn advance_time(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    /// Get the current time from the test clock.
    pub fn now(&self) -> std::time::Instant {
        self.clock.now()
    }

    /// Get the current system time from the test clock.
    pub fn now_system(&self) -> std::time::SystemTime {
        self.clock.now_system()
    }

    /// Get elapsed time since the test clock was created.
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Create a test tenant with default configuration.
    pub async fn create_tenant(&mut self, name: &str) -> Result<TenantId> {
        self.create_tenant_with_plan(name, "enterprise").await
    }

    /// Create a test tenant with specific plan.
    pub async fn create_tenant_with_plan(&mut self, name: &str, plan: &str) -> Result<TenantId> {
        let tenant_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO tenants (id, name, plan, api_key, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NOW(), NOW())",
        )
        .bind(tenant_id)
        .bind(name)
        .bind(plan)
        .bind(format!("test-key-{}", tenant_id.simple()))
        .execute(&mut **self.db())
        .await
        .context("failed to create test tenant")?;

        Ok(TenantId(tenant_id))
    }

    /// Create a test endpoint with default configuration.
    pub async fn create_endpoint(&mut self, tenant_id: TenantId, url: &str) -> Result<EndpointId> {
        self.create_endpoint_with_config(tenant_id, url, "test-endpoint", 10, 30).await
    }

    /// Create a test endpoint with full configuration.
    pub async fn create_endpoint_with_config(
        &mut self,
        tenant_id: TenantId,
        url: &str,
        name: &str,
        max_retries: i32,
        timeout_seconds: i32,
    ) -> Result<EndpointId> {
        let endpoint_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO endpoints (id, tenant_id, url, name, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"
        )
        .bind(endpoint_id)
        .bind(tenant_id.0)
        .bind(url)
        .bind(name)
        .bind(max_retries)
        .bind(timeout_seconds)
        .bind("closed")
        .execute(&mut **self.db())
        .await
        .context("failed to create test endpoint")?;

        Ok(EndpointId(endpoint_id))
    }

    /// Seed common test data.
    ///
    /// Creates a standard tenant and endpoint for tests that don't
    /// need specific configurations.
    pub async fn seed_test_data(&mut self) -> Result<(TenantId, EndpointId)> {
        let tenant_id = self.create_tenant("test-tenant").await?;
        let endpoint_id = self.create_endpoint(tenant_id, "https://example.com/webhook").await?;
        Ok((tenant_id, endpoint_id))
    }

    /// Ingests a test webhook, persisting it to the database transaction.
    ///
    /// Handles idempotency by returning existing event ID if duplicate
    /// source_event_id.
    pub async fn ingest_webhook(&mut self, webhook: &TestWebhook) -> Result<EventId> {
        let event_id = Uuid::new_v4();
        let body_bytes = webhook.body.clone();
        let payload_size = (body_bytes.len() as i32).max(1); // Ensure minimum size of 1

        let result: (Uuid,) = sqlx::query_as(
            "INSERT INTO webhook_events
             (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
              status, failure_count, headers, body, content_type, payload_size, received_at)
             VALUES ($1, $2, $3, $4, $5, 'pending', 0, $6, $7, $8, $9, $10)
             ON CONFLICT (tenant_id, endpoint_id, source_event_id)
             DO UPDATE SET id = webhook_events.id
             RETURNING id",
        )
        .bind(event_id)
        .bind(webhook.tenant_id)
        .bind(webhook.endpoint_id)
        .bind(&webhook.source_event_id)
        .bind(&webhook.idempotency_strategy)
        .bind(serde_json::to_value(&webhook.headers)?)
        .bind(&body_bytes[..])
        .bind(&webhook.content_type)
        .bind(payload_size)
        .bind(chrono::DateTime::<Utc>::from(self.now_system()))
        .fetch_one(&mut **self.db())
        .await
        .context("failed to ingest test webhook")?;

        Ok(EventId(result.0))
    }

    /// Gets the current status of a webhook event.
    pub async fn get_webhook_status(&mut self, event_id: EventId) -> Result<String> {
        let status: (String,) = sqlx::query_as("SELECT status FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(&mut **self.db())
            .await
            .context("failed to get webhook status")?;
        Ok(status.0)
    }

    /// Gets the number of delivery attempts for a webhook event.
    pub async fn get_delivery_attempts_count(&mut self, event_id: EventId) -> Result<i64> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
                .bind(event_id.0)
                .fetch_one(&mut **self.db())
                .await
                .context("failed to get delivery attempts count")?;
        Ok(count.0)
    }

    /// Simulates a single run of the delivery worker pool.
    ///
    /// Finds pending webhooks ready for delivery and processes them.
    pub async fn run_delivery_cycle(&mut self) -> Result<()> {
        // Find webhooks ready for delivery
        let ready_webhooks: Vec<(EventId, Uuid, String, Vec<u8>, i32, String)> = sqlx::query_as(
            "SELECT we.id, e.id as endpoint_id, e.url, we.body, we.failure_count, e.name
             FROM webhook_events we
             JOIN endpoints e ON we.endpoint_id = e.id
             WHERE we.status = 'pending'
             AND (we.next_retry_at IS NULL OR we.next_retry_at <= $1)
             ORDER BY we.received_at
             LIMIT 10",
        )
        .bind(chrono::DateTime::<Utc>::from(self.now_system()))
        .fetch_all(&mut **self.db())
        .await
        .context("failed to fetch ready webhooks")?;

        let webhook_count = ready_webhooks.len();
        for (event_id, _endpoint_id, url, body, failure_count, _endpoint_name) in ready_webhooks {
            // Make HTTP request to the mock server
            let client = reqwest::Client::new();
            let response_result = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("X-Kapsel-Event-Id", event_id.0.to_string())
                .body(body.clone())
                .send()
                .await;

            let (status_code, response_body, duration_ms, error_type) = match response_result {
                Ok(response) => {
                    let status = response.status().as_u16() as i32;
                    let body = response.text().await.unwrap_or_default();
                    let error_type = if status >= 500 {
                        Some("http_error")
                    } else if status >= 400 {
                        Some("client_error")
                    } else {
                        None
                    };
                    (Some(status), Some(body), 75i32, error_type)
                },
                Err(_) => (None, None, 1000i32, Some("network_error")),
            };

            // Record delivery attempt
            let attempt_number = failure_count + 1;
            let attempt_id = Uuid::new_v4();
            let attempted_at = chrono::DateTime::<Utc>::from(self.now_system());

            sqlx::query(
                "INSERT INTO delivery_attempts
                 (id, event_id, attempt_number, request_url, request_headers,
                  response_status, response_body, attempted_at, duration_ms, error_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(attempt_id)
            .bind(event_id.0)
            .bind(attempt_number)
            .bind(&url)
            .bind(serde_json::json!({"X-Kapsel-Event-Id": event_id.0.to_string()}))
            .bind(status_code)
            .bind(response_body)
            .bind(attempted_at)
            .bind(duration_ms)
            .bind(error_type)
            .execute(&mut **self.db())
            .await
            .context("failed to record delivery attempt")?;

            // Update webhook status based on response
            let (new_status, next_retry_at) = match status_code {
                Some(200..=299) => ("delivered".to_string(), None),
                _ => {
                    let backoff_delay =
                        time::backoff::deterministic_webhook_backoff(failure_count as u32);
                    let current_time = chrono::DateTime::<Utc>::from(self.now_system());
                    let next_retry =
                        Some(current_time + chrono::Duration::from_std(backoff_delay)?);
                    ("pending".to_string(), next_retry)
                },
            };

            if new_status == "delivered" {
                sqlx::query(
                    "UPDATE webhook_events
                     SET status = $1, delivered_at = $2, failure_count = $3, last_attempt_at = $4
                     WHERE id = $5",
                )
                .bind(new_status)
                .bind(attempted_at)
                .bind(attempt_number)
                .bind(attempted_at)
                .bind(event_id.0)
                .execute(&mut **self.db())
                .await?;
            } else {
                sqlx::query(
                    "UPDATE webhook_events
                     SET status = $1, failure_count = $2, last_attempt_at = $3, next_retry_at = $4
                     WHERE id = $5",
                )
                .bind(new_status)
                .bind(attempt_number)
                .bind(attempted_at)
                .bind(next_retry_at)
                .bind(event_id.0)
                .execute(&mut **self.db())
                .await?;
            }
        }

        tracing::debug!("Processed {} webhooks in delivery cycle", webhook_count);
        tokio::task::yield_now().await;
        Ok(())
    }

    /// Count rows in a table by ID.
    pub async fn count_by_id(&mut self, table: &str, id_column: &str, id: Uuid) -> Result<i64> {
        let query = format!("SELECT COUNT(*) as count FROM {} WHERE {} = $1", table, id_column);

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_one(&mut **self.db())
            .await
            .context("failed to count rows")?;

        let count: i64 = row.try_get("count")?;
        Ok(count)
    }

    /// Check if database connection is healthy.
    pub async fn database_health_check(&mut self) -> Result<bool> {
        let result = sqlx::query("SELECT 1 as health").fetch_one(&mut **self.db()).await;
        Ok(result.is_ok())
    }

    /// List all tables in the database schema.
    pub async fn list_tables(&mut self) -> Result<Vec<String>> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public'
             AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .fetch_all(&mut **self.db())
        .await
        .context("failed to list tables")?;

        Ok(tables)
    }
}

/// Test scenario builder for complex multi-step tests.
///
/// Enables deterministic testing with time control and invariant validation.
pub struct ScenarioBuilder {
    name: String,
    steps: Vec<Step>,
    invariant_checks: Vec<InvariantCheckFn>,
}

enum Step {
    IngestWebhook(TestWebhook),
    RunDeliveryCycle,
    AdvanceTime(Duration),
    InjectHttpFailure(http::MockResponse),
    AssertState(AssertionFn),
    ExpectStatus(EventId, String),
    ExpectDeliveryAttempts(EventId, i64),
}

#[derive(Debug, Clone)]
pub enum FailureKind {
    NetworkTimeout,
    Http500,
    Http429 { retry_after: Duration },
    DatabaseUnavailable,
}

impl ScenarioBuilder {
    /// Create a new test scenario.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), steps: Vec::new(), invariant_checks: Vec::new() }
    }

    /// Add a webhook ingestion step.
    pub fn ingest(mut self, webhook: TestWebhook) -> Self {
        self.steps.push(Step::IngestWebhook(webhook));
        self
    }

    /// Run a delivery cycle to process pending webhooks.
    pub fn run_delivery_cycle(mut self) -> Self {
        self.steps.push(Step::RunDeliveryCycle);
        self
    }

    /// Advance test time.
    pub fn advance_time(mut self, duration: Duration) -> Self {
        self.steps.push(Step::AdvanceTime(duration));
        self
    }

    /// Inject an HTTP failure response.
    pub fn inject_http_failure(mut self, response: http::MockResponse) -> Self {
        self.steps.push(Step::InjectHttpFailure(response));
        self
    }

    /// Expect a webhook to have a specific status.
    pub fn expect_status(mut self, event_id: EventId, expected_status: impl Into<String>) -> Self {
        self.steps.push(Step::ExpectStatus(event_id, expected_status.into()));
        self
    }

    /// Expect a specific number of delivery attempts.
    pub fn expect_delivery_attempts(mut self, event_id: EventId, expected_count: i64) -> Self {
        self.steps.push(Step::ExpectDeliveryAttempts(event_id, expected_count));
        self
    }

    /// Add a custom assertion.
    pub fn assert_state<F>(mut self, assertion: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.steps.push(Step::AssertState(Box::new(assertion)));
        self
    }

    /// Add an invariant check that runs after each step.
    pub fn check_invariant<F>(mut self, check: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.invariant_checks.push(Box::new(check));
        self
    }

    /// Execute the scenario.
    pub async fn run(self, env: &mut TestEnv) -> Result<()> {
        tracing::info!("running scenario: {}", self.name);

        // Use a HashMap to store event IDs created during the scenario
        let mut event_ids: HashMap<String, EventId> = HashMap::new();

        for (i, step) in self.steps.into_iter().enumerate() {
            tracing::debug!("executing step {}", i + 1);

            match step {
                Step::IngestWebhook(webhook) => {
                    let source_id = webhook.source_event_id.clone();
                    tracing::debug!("ingesting webhook with source_id {}", source_id);
                    let event_id = env.ingest_webhook(&webhook).await?;
                    event_ids.insert(source_id, event_id);
                },
                Step::RunDeliveryCycle => {
                    tracing::debug!("running delivery cycle");
                    env.run_delivery_cycle().await?;
                },
                Step::AdvanceTime(duration) => {
                    env.advance_time(duration);
                    tracing::debug!("advanced time by {:?}", duration);
                },
                Step::InjectHttpFailure(_response) => {
                    tracing::debug!("injecting HTTP failure response");
                    // This would configure the mock server with the failure
                    // response For now, this is a
                    // placeholder
                },
                Step::AssertState(assertion) => {
                    assertion(env).context("state assertion failed")?;
                },
                Step::ExpectStatus(event_id, expected) => {
                    let actual = env.get_webhook_status(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Webhook status mismatch for event {}: expected '{}', got '{}'",
                        event_id.0, expected, actual
                    );
                },
                Step::ExpectDeliveryAttempts(event_id, expected) => {
                    let actual = env.get_delivery_attempts_count(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Delivery attempt count mismatch for event {}: expected {}, got {}",
                        event_id.0, expected, actual
                    );
                },
            }

            // Run invariant checks after each step
            for (check_idx, check) in self.invariant_checks.iter().enumerate() {
                check(env).with_context(|| {
                    format!(
                        "invariant check {} failed after step {} in scenario '{}'",
                        check_idx + 1,
                        i + 1,
                        self.name
                    )
                })?;
            }
        }

        Ok(())
    }
}

/// Common test assertions for webhook verification.
pub mod assertions {
    use bytes::Bytes;
    use serde_json::Value;

    /// Assert that JSON payloads match.
    pub fn assert_json_matches(actual: &Bytes, expected: &Value) {
        let actual_json: Value =
            serde_json::from_slice(actual).expect("failed to parse actual JSON");

        assert_eq!(
            actual_json,
            *expected,
            "JSON mismatch\nActual: {}\nExpected: {}",
            serde_json::to_string_pretty(&actual_json).unwrap(),
            serde_json::to_string_pretty(expected).unwrap()
        );
    }

    /// Assert webhook was delivered within timeout.
    pub async fn assert_delivered_within(_event_id: &str, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Check delivery status in database
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Placeholder - actual implementation would check database
            if false {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_env_provides_transaction_isolation() {
        // Each test gets its own isolated transaction
        let mut env1 = TestEnv::new().await.unwrap();
        let mut env2 = TestEnv::new().await.unwrap();

        // Create data in first environment
        let _tenant1 = env1.create_tenant("env1-tenant").await.unwrap();

        // Query in first environment - should find it
        let count1: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = 'env1-tenant'")
                .fetch_one(&mut **env1.db())
                .await
                .unwrap();
        assert_eq!(count1, 1);

        // Query in second environment - should not find it
        let count2: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = 'env1-tenant'")
                .fetch_one(&mut **env2.db())
                .await
                .unwrap();
        assert_eq!(count2, 0, "data should not leak between test environments");

        // Create different data in second environment
        let _tenant2 = env2.create_tenant("env2-tenant").await.unwrap();

        // Verify isolation is maintained
        let count1_again: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = 'env2-tenant'")
                .fetch_one(&mut **env1.db())
                .await
                .unwrap();
        assert_eq!(count1_again, 0);
    }

    #[tokio::test]
    async fn scenario_builder_executes_steps() {
        let mut env = TestEnv::new().await.unwrap();

        let scenario = ScenarioBuilder::new("test scenario")
            .advance_time(Duration::from_secs(1))
            .assert_state(|env| {
                // Custom assertion
                assert!(env.elapsed() >= Duration::from_secs(1));
                Ok(())
            })
            .advance_time(Duration::from_secs(2))
            .assert_state(|env| {
                // Verify cumulative time
                assert!(env.elapsed() >= Duration::from_secs(3));
                Ok(())
            })
            .check_invariant(|_env| {
                // Invariant check runs after each step
                Ok(())
            });

        scenario.run(&mut env).await.unwrap();
    }

    #[tokio::test]
    async fn test_env_seed_data_works() {
        let mut env = TestEnv::new().await.unwrap();

        let (tenant_id, endpoint_id) = env.seed_test_data().await.unwrap();

        // Verify tenant exists
        let tenant_count = env.count_by_id("tenants", "id", tenant_id.0).await.unwrap();
        assert_eq!(tenant_count, 1);

        // Verify endpoint exists
        let endpoint_count = env.count_by_id("endpoints", "id", endpoint_id.0).await.unwrap();
        assert_eq!(endpoint_count, 1);
    }

    #[tokio::test]
    async fn health_check_works() {
        let mut env = TestEnv::new().await.unwrap();
        assert!(env.database_health_check().await.unwrap());
    }

    #[tokio::test]
    async fn list_tables_returns_schema() {
        let mut env = TestEnv::new().await.unwrap();
        let tables = env.list_tables().await.unwrap();

        // Verify key tables exist
        assert!(tables.contains(&"tenants".to_string()));
        assert!(tables.contains(&"endpoints".to_string()));
        assert!(tables.contains(&"webhook_events".to_string()));
        assert!(tables.contains(&"delivery_attempts".to_string()));
    }
}
