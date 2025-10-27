//! Core TestEnv implementation - basic environment setup and management

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use kapsel_core::{storage::Storage, Clock};
use uuid::Uuid;

use crate::{database::TestDatabase, http, time, TestEnv};

impl TestEnv {
    /// Create test environment with shared database and transaction isolation.
    pub async fn new() -> Result<Self> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let database =
            TestDatabase::new().await.context("failed to create test database")?.pool().clone();

        let test_run_id = Uuid::new_v4().simple().to_string();

        let http_mock = http::MockServer::start().await;
        let clock = time::TestClock::new();
        let storage = Arc::new(Storage::new(database.clone()));

        Ok(Self {
            http_mock,
            clock,
            database,
            storage,
            attestation_service: None,
            test_run_id,
            is_isolated: false,
        })
    }

    /// Create test environment with isolated database for tests requiring
    /// committed data.
    pub async fn new_isolated() -> Result<Self> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let database = TestDatabase::new_isolated()
            .await
            .context("failed to create isolated test database")?
            .pool()
            .clone();

        // Generate unique test run ID for data isolation
        let test_run_id = Uuid::new_v4().simple().to_string();

        let http_mock = http::MockServer::start().await;
        let clock = time::TestClock::new();
        let storage = Arc::new(Storage::new(database.clone()));

        Ok(Self {
            http_mock,
            clock,
            database,
            storage,
            attestation_service: None,
            test_run_id,
            is_isolated: true,
        })
    }

    /// Verify database connection is healthy and schema is correctly set.
    pub async fn verify_connection(&self) -> Result<()> {
        // Check basic connectivity
        let result: (i32,) = sqlx::query_as("SELECT 1")
            .fetch_one(self.pool())
            .await
            .context("database connection test failed")?;

        if result.0 != 1 {
            anyhow::bail!("database connection check returned unexpected value");
        }

        // For isolated databases, verify schema is set correctly
        if self.is_isolated {
            let (search_path,): (String,) = sqlx::query_as("SHOW search_path")
                .fetch_one(self.pool())
                .await
                .context("failed to get search_path")?;

            tracing::debug!("Current search_path: {}", search_path);

            // Check if we can access our tables
            let tables_count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM information_schema.tables
                 WHERE table_schema = current_schema()
                 AND table_name IN ('webhook_events', 'endpoints', 'tenants')",
            )
            .fetch_one(self.pool())
            .await
            .context("failed to check table access")?;

            if tables_count.0 < 3 {
                anyhow::bail!(
                    "Missing required tables in current schema. Found {} tables, expected at least 3",
                    tables_count.0
                );
            }
        }

        Ok(())
    }

    /// Returns direct access to the database connection pool.
    ///
    /// # Database-Per-Test Isolation
    ///
    /// Tests can use this pool directly - no transactions needed for isolation:
    ///
    /// ```rust,no_run
    /// # use kapsel_testing::TestEnv;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let env = TestEnv::new_isolated().await?;
    /// sqlx::query("INSERT INTO tenants (name) VALUES ($1)")
    ///     .bind("test-tenant")
    ///     .execute(env.pool())
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Each test gets its own isolated database cloned from a template,
    /// so changes in one test never affect another test.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.database
    }

    /// Returns access to the storage layer repositories.
    ///
    /// This provides access to the production storage repositories,
    /// ensuring tests exercise the same code paths that run in production.
    pub fn storage(&self) -> Arc<Storage> {
        self.storage.clone()
    }

    /// Create a new connection pool for components that manage their own
    /// connections.
    ///
    /// This is useful for testing delivery workers and other components
    /// that need their own database connections while maintaining isolation.
    pub fn create_pool(&self) -> sqlx::PgPool {
        self.database.clone()
    }

    /// Advance the test clock by the specified duration.
    pub fn advance_time(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    /// Returns the current time from the test clock.
    pub fn now(&self) -> std::time::Instant {
        self.clock.now()
    }

    /// Returns the current system time from the test clock.
    pub fn now_system(&self) -> std::time::SystemTime {
        self.clock.now_system()
    }

    /// Returns elapsed time since the test clock was created.
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Detects if this is an isolated test environment.
    ///
    /// Used internally to handle different attestation key strategies.
    /// Isolated tests have their own database.
    pub fn is_isolated_test(&self) -> bool {
        self.is_isolated
    }

    /// Generate unique advisory lock ID for test isolation.
    ///
    /// Creates a deterministic lock ID based on the test run ID to ensure
    /// concurrent tests don't compete for the same PostgreSQL advisory lock.
    pub(crate) fn generate_unique_lock_id(&self) -> i64 {
        use std::{
            collections::hash_map::DefaultHasher,
            hash::{Hash, Hasher},
        };

        let mut hasher = DefaultHasher::new();
        self.test_run_id.hash(&mut hasher);
        // Use the hash as a positive i64 for the advisory lock
        (hasher.finish() as i64).abs()
    }

    /// Debug helper to check database pool statistics.
    pub async fn debug_pool_stats(&self) -> String {
        let pool = self.pool();
        format!(
            "Pool stats: size={}, num_idle={}, is_closed={}",
            pool.size(),
            pool.num_idle(),
            pool.is_closed()
        )
    }

    /// Debug helper to list all webhook events in the database.
    pub async fn debug_list_events(&self) -> Result<Vec<(Uuid, String, String)>> {
        let events = self
            .storage()
            .webhook_events
            .list_all()
            .await
            .context("failed to list webhook events")?;

        let events: Vec<(Uuid, String, String)> = events
            .into_iter()
            .rev() // Reverse to match DESC order
            .take(20) // Limit to 20 events
            .map(|e| (e.id.0, e.source_event_id, e.status.to_string()))
            .collect();

        tracing::debug!("Found {} webhook events in database", events.len());
        for (id, source, status) in &events {
            tracing::debug!("  Event {}: source={}, status={}", id, source, status);
        }

        Ok(events)
    }

    /// Enable attestation service for testing.
    pub fn enable_attestation(&mut self, service: kapsel_attestation::MerkleService) {
        use std::sync::Arc;

        use tokio::sync::RwLock;

        self.attestation_service = Some(Arc::new(RwLock::new(service)));
    }
}
