//! Core TestEnv implementation - basic environment setup and manage//! Core
//! TestEnv implementation - basic environment setup and management

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use kapsel_attestation::{AttestationEventSubscriber, MerkleService};
use kapsel_core::{storage::Storage, Clock};
use kapsel_delivery::{
    retry::{BackoffStrategy, RetryPolicy},
    ClientConfig, DeliveryConfig, DeliveryEngine,
};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{database::TestDatabase, http, TestClock, TestEnv};

/// Builder for configuring TestEnv with production engines.
pub struct TestEnvBuilder {
    worker_count: usize,
    batch_size: usize,
    poll_interval: Duration,
    shutdown_timeout: Duration,
    enable_delivery_engine: bool,
    is_isolated: bool,
}

impl Default for TestEnvBuilder {
    fn default() -> Self {
        Self {
            worker_count: 1, // Single worker for determinism
            batch_size: 10,
            poll_interval: Duration::from_millis(100),
            shutdown_timeout: Duration::from_secs(5),
            enable_delivery_engine: true,
            is_isolated: true,
        }
    }
}

impl TestEnvBuilder {
    /// Creates a new builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the number of delivery workers (default: 1 for determinism).
    #[must_use]
    pub fn worker_count(mut self, count: usize) -> Self {
        self.worker_count = count;
        self
    }

    /// Sets the batch size for claiming events (default: 10).
    #[must_use]
    pub fn batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Sets the poll interval for workers (default: 100ms).
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Sets the shutdown timeout for graceful termination (default: 5s).
    #[must_use]
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Disables the production delivery engine for tests that don't need it.
    #[must_use]
    pub fn without_delivery_engine(mut self) -> Self {
        self.enable_delivery_engine = false;
        self
    }

    /// Use isolated database for this test environment.
    ///
    /// Creates a dedicated PostgreSQL database for this test. Each database
    /// consumes ~8MB memory. Only use when tests require background workers,
    /// API endpoints, or testing COMMIT behavior.
    #[must_use]
    pub fn isolated(mut self) -> Self {
        self.is_isolated = true;
        self
    }

    /// Use shared database for this test environment (default).
    ///
    /// This is already the default behavior. Method provided for explicitness.
    #[must_use]
    pub fn shared(mut self) -> Self {
        self.is_isolated = false;
        self
    }

    /// Builds the test environment with configured production engines.
    pub async fn build(self) -> Result<TestEnv> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let database = if self.is_isolated {
            TestDatabase::new_isolated()
                .await
                .context("failed to create isolated test database")?
                .pool()
                .clone()
        } else {
            TestDatabase::new().await.context("failed to create test database")?.pool().clone()
        };

        let test_run_id = Uuid::new_v4().simple().to_string();
        let http_mock = http::MockServer::start().await;
        let clock = TestClock::new();
        let clock_arc: Arc<dyn Clock> = Arc::new(clock.clone());
        let storage = Arc::new(Storage::new(database.clone(), &clock_arc));

        let delivery_engine = if self.enable_delivery_engine {
            let delivery_config = DeliveryConfig {
                worker_count: self.worker_count,
                batch_size: self.batch_size,
                poll_interval: self.poll_interval,
                shutdown_timeout: self.shutdown_timeout,
                client_config: ClientConfig::default(),
                default_retry_policy: RetryPolicy {
                    max_attempts: 10,
                    base_delay: Duration::from_secs(1),
                    max_delay: Duration::from_secs(600),
                    jitter_factor: 0.0, // Zero jitter for deterministic tests
                    backoff_strategy: BackoffStrategy::Exponential,
                },
            };

            let clock_arc: Arc<dyn Clock> = Arc::new(clock.clone());

            // Use NoOpEventHandler by default for test isolation
            // This avoids coupling delivery tests to the attestation system
            let event_handler =
                Arc::new(kapsel_core::NoOpEventHandler) as Arc<dyn kapsel_core::EventHandler>;

            Some(
                DeliveryEngine::with_event_handler(
                    database.clone(),
                    delivery_config,
                    clock_arc,
                    event_handler,
                )
                .context("failed to create delivery engine")?,
            )
        } else {
            None
        };

        Ok(TestEnv {
            http_mock,
            clock,
            database,
            storage,
            attestation_service: None,
            test_run_id,
            is_isolated: self.is_isolated,
            delivery_engine,
        })
    }
}

impl TestEnv {
    /// Create test environment with ISOLATED database.
    ///
    /// Creates a dedicated PostgreSQL database for this test. This is the
    /// default behavior for backward compatibility with existing tests.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use kapsel_testing::TestEnv;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let env = TestEnv::new().await?;
    /// let webhook = kapsel_testing::fixtures::WebhookBuilder::new()
    ///     .tenant(uuid::Uuid::new_v4())
    ///     .endpoint(uuid::Uuid::new_v4())
    ///     .build();
    ///
    /// // Can commit directly to the isolated database
    /// let event = env.ingest_webhook(&webhook).await?;
    ///
    /// // Background workers can see the committed data
    /// env.process_batch().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new() -> Result<Self> {
        TestEnvBuilder::new().build().await
    }

    /// Create test environment with SHARED database pool.
    ///
    /// Uses a single shared database (`kapsel_test`) with transaction-based
    /// isolation. Tests should begin a transaction and let it roll back at
    /// the end for automatic cleanup. Recommended for repository/service tests
    /// that don't need background workers.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use kapsel_testing::TestEnv;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let env = TestEnv::new_shared().await?;
    /// let mut tx = env.pool().begin().await?;
    ///
    /// // All operations use the transaction
    /// let tenant = env.create_tenant_tx(&mut tx, "test").await?;
    ///
    /// // Transaction automatically rolls back when dropped
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new_shared() -> Result<Self> {
        TestEnvBuilder::new().shared().build().await
    }

    /// Create test environment with fully ISOLATED database.
    ///
    /// Alias for `new()` for explicitness. Creates a dedicated PostgreSQL
    /// database for this test.
    pub async fn new_isolated() -> Result<Self> {
        Self::new().await
    }

    /// Creates a builder for advanced TestEnv configuration.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use kapsel_testing::TestEnv;
    /// # use std::time::Duration;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let env = TestEnv::builder()
    ///     .worker_count(2)
    ///     .batch_size(20)
    ///     .poll_interval(Duration::from_millis(50))
    ///     .isolated()
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> TestEnvBuilder {
        TestEnvBuilder::new()
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
        i64::try_from(hasher.finish()).unwrap_or(i64::MAX).abs()
    }

    /// Debug helper to check database pool statistics.
    pub fn debug_pool_stats(&self) -> String {
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
    ///
    /// This rebuilds the delivery engine with attestation event handler to
    /// ensure delivery attempts are captured for attestation.
    pub fn enable_attestation(&mut self, service: MerkleService) -> anyhow::Result<()> {
        tracing::debug!("Enabling attestation service - rebuilding delivery engine");

        // Store the attestation service
        let attestation_service_arc = Arc::new(RwLock::new(service));
        self.attestation_service = Some(attestation_service_arc.clone());

        // Rebuild delivery engine with attestation if it exists
        if let Some(_old_engine) = self.delivery_engine.take() {
            tracing::debug!("Took old delivery engine, creating new one with attestation");

            // Create attestation event handler
            let attestation_subscriber = AttestationEventSubscriber::new(attestation_service_arc);

            // Create new delivery engine with same configuration but with attestation
            let delivery_config = DeliveryConfig {
                worker_count: 1,
                batch_size: 10,
                poll_interval: std::time::Duration::from_millis(100),
                client_config: kapsel_delivery::ClientConfig::default(),
                default_retry_policy: kapsel_delivery::retry::RetryPolicy {
                    jitter_factor: 0.0, // Deterministic for tests
                    ..Default::default()
                },
                shutdown_timeout: std::time::Duration::from_secs(5),
            };

            let new_engine = DeliveryEngine::with_event_handler(
                self.database.clone(),
                delivery_config,
                Arc::new(self.clock.clone()) as Arc<dyn Clock>,
                Arc::new(attestation_subscriber),
            )?;

            // Install the new engine
            self.delivery_engine = Some(new_engine);
            tracing::debug!("Successfully created new delivery engine with attestation");
        }

        Ok(())
    }
}
