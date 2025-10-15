//! Test harness for Kapsel integration and unit tests.
//!
//! Provides deterministic test infrastructure with transaction-based database
//! isolation, HTTP mocking, and fixture builders for reliable testing.

pub mod database;
pub mod fixtures;
pub mod http;
pub mod invariants;
pub mod time;

use std::time::Duration;

use anyhow::{Context, Result};
use database::TestDatabase;
pub use invariants::{assertions as invariant_assertions, strategies, Invariants};
use kapsel_core::models::{EndpointId, TenantId};
use sqlx::{PgPool, Postgres, Row};
pub use time::Clock;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

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
    /// Database connection pool for components that need their own connections
    pool: PgPool,
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
        let db = TestDatabase::new().await?;
        let pool = db.pool();
        let tx = db.begin_transaction().await?;

        // Create HTTP mock server
        let http_mock = http::MockServer::start().await;

        // Create deterministic clock
        let clock = time::TestClock::new();

        Ok(Self { tx: Some(tx), http_mock, clock, pool })
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
        self.pool.clone()
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

    /// Create a test tenant with default configuration.
    pub async fn create_tenant(&mut self, name: &str) -> Result<TenantId> {
        self.create_tenant_with_plan(name, "enterprise").await
    }

    /// Create a test tenant with specific plan.
    pub async fn create_tenant_with_plan(&mut self, name: &str, plan: &str) -> Result<TenantId> {
        let tenant_id = Uuid::new_v4();

        sqlx::query!(
            "INSERT INTO tenants (id, name, plan, api_key, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NOW(), NOW())",
            tenant_id,
            name,
            plan,
            format!("test-key-{}", tenant_id.simple())
        )
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

        sqlx::query!(
            "INSERT INTO endpoints (id, tenant_id, url, name, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())",
            endpoint_id,
            tenant_id.0,
            url,
            name,
            max_retries,
            timeout_seconds,
            "closed"
        )
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
    invariant_checks: Vec<Box<dyn Fn(&mut TestEnv) -> Result<()>>>,
}

enum Step {
    IngestWebhook { endpoint_id: String, payload: bytes::Bytes },
    ExpectDelivery { timeout: Duration },
    InjectFailure { kind: FailureKind },
    AdvanceTime { duration: Duration },
    AssertState { assertion: Box<dyn Fn(&mut TestEnv) -> Result<()>> },
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
    pub fn ingest(mut self, endpoint_id: impl Into<String>, payload: bytes::Bytes) -> Self {
        self.steps.push(Step::IngestWebhook { endpoint_id: endpoint_id.into(), payload });
        self
    }

    /// Expect delivery within timeout.
    pub fn expect_delivery(mut self, timeout: Duration) -> Self {
        self.steps.push(Step::ExpectDelivery { timeout });
        self
    }

    /// Inject a failure condition.
    pub fn inject_failure(mut self, kind: FailureKind) -> Self {
        self.steps.push(Step::InjectFailure { kind });
        self
    }

    /// Advance test time.
    pub fn advance_time(mut self, duration: Duration) -> Self {
        self.steps.push(Step::AdvanceTime { duration });
        self
    }

    /// Add a custom assertion.
    pub fn assert_state<F>(mut self, assertion: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.steps.push(Step::AssertState { assertion: Box::new(assertion) });
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

        for (i, step) in self.steps.into_iter().enumerate() {
            tracing::debug!("executing step {}", i + 1);

            match step {
                Step::IngestWebhook { endpoint_id, payload: _ } => {
                    tracing::debug!("ingesting webhook for endpoint {}", endpoint_id);
                    // Actual ingestion logic would go here
                },
                Step::ExpectDelivery { timeout } => {
                    tracing::debug!("waiting for delivery within {:?}", timeout);
                    // Delivery verification would go here
                },
                Step::InjectFailure { kind } => {
                    tracing::debug!("injecting failure: {:?}", kind);
                    // Mock configuration would go here
                },
                Step::AdvanceTime { duration } => {
                    env.advance_time(duration);
                    tracing::debug!("advanced time by {:?}", duration);
                },
                Step::AssertState { assertion } => {
                    assertion(env).context("state assertion failed")?;
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
        let tenant1 = env1.create_tenant("env1-tenant").await.unwrap();

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
            .ingest("endpoint_1", bytes::Bytes::from("test payload"))
            .advance_time(Duration::from_secs(1))
            .expect_delivery(Duration::from_secs(5))
            .assert_state(|env| {
                // Custom assertion
                assert!(env.clock.now().elapsed() >= Duration::from_secs(1));
                Ok(())
            })
            .check_invariant(|_env| {
                // Invariant check
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
