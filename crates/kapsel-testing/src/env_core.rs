//! Core TestEnv implementation - basic environment setup and management

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures::FutureExt;
use kapsel_attestation::{AttestationEventSubscriber, MerkleService};
use kapsel_core::{storage::Storage, Clock, EventHandler, NoOpEventHandler};
use kapsel_delivery::{
    retry::{BackoffStrategy, RetryPolicy},
    storage::PostgresDeliveryStorage,
    ClientConfig, DeliveryConfig, DeliveryEngine, DeliveryStorage,
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{database, database::TestDatabase, http, time, TestClock, TestEnv};

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
            enable_delivery_engine: false, // Disabled by default for performance
            is_isolated: false,            // Default to shared database to prevent leaks
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
        let start_time = Instant::now();

        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let db_start = Instant::now();
        let database = if self.is_isolated {
            TestDatabase::new_isolated()
                .await
                .context("failed to create isolated test database")?
                .pool()
                .clone()
        } else {
            TestDatabase::new().await.context("failed to create test database")?.pool().clone()
        };
        let db_duration = db_start.elapsed();

        let test_run_id = Uuid::new_v4().simple().to_string();

        let http_start = Instant::now();
        let http_mock = http::MockServer::start().await;
        let http_duration = http_start.elapsed();

        let clock = TestClock::new();
        let clock_arc: Arc<dyn Clock> = Arc::new(clock.clone());
        let storage = Arc::new(Storage::new(database.clone(), &clock_arc));

        let delivery_start = Instant::now();
        let delivery_engine = if self.enable_delivery_engine {
            let delivery_config = DeliveryConfig {
                worker_count: self.worker_count,
                batch_size: self.batch_size,
                poll_interval: self.poll_interval,
                shutdown_timeout: self.shutdown_timeout,
                client_config: ClientConfig {
                    timeout: Duration::from_secs(2),
                    user_agent: "Kapsel-Test/1.0".to_string(),
                    max_redirects: 2,
                    verify_tls: false,
                },
                default_retry_policy: RetryPolicy {
                    max_attempts: 2,
                    base_delay: Duration::from_millis(100),
                    max_delay: Duration::from_secs(5),
                    jitter_factor: 0.0, // Zero jitter for deterministic tests
                    backoff_strategy: BackoffStrategy::Exponential,
                },
            };

            let clock_arc: Arc<dyn Clock> = Arc::new(clock.clone());

            // Use NoOpEventHandler by default for test isolation
            // This avoids coupling delivery tests to the attestation system
            let event_handler = Arc::new(NoOpEventHandler) as Arc<dyn kapsel_core::EventHandler>;

            let concrete_storage = Arc::new(Storage::new(database.clone(), &clock_arc));
            let delivery_storage: Arc<dyn kapsel_delivery::storage::DeliveryStorage> =
                Arc::new(PostgresDeliveryStorage::new(concrete_storage));
            Some(
                DeliveryEngine::with_event_handler(
                    delivery_storage,
                    delivery_config,
                    clock_arc,
                    event_handler,
                )
                .context("failed to create delivery engine")?,
            )
        } else {
            None
        };
        let delivery_duration = delivery_start.elapsed();

        let total_duration = start_time.elapsed();

        if total_duration > Duration::from_millis(500) {
            warn!(
                "TestEnv::build() took {}ms (DB: {}ms, HTTP: {}ms, Delivery: {}ms, isolated: {})",
                total_duration.as_millis(),
                db_duration.as_millis(),
                http_duration.as_millis(),
                delivery_duration.as_millis(),
                self.is_isolated
            );
        } else {
            debug!(
                "TestEnv::build() completed in {}ms (isolated: {})",
                total_duration.as_millis(),
                self.is_isolated
            );
        }

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
                client_config: kapsel_delivery::ClientConfig {
                    timeout: Duration::from_secs(2),
                    user_agent: "Kapsel-Test/1.0".to_string(),
                    max_redirects: 2,
                    verify_tls: false,
                },
                default_retry_policy: kapsel_delivery::retry::RetryPolicy {
                    jitter_factor: 0.0, // Deterministic for tests
                    ..Default::default()
                },
                shutdown_timeout: std::time::Duration::from_secs(1),
            };

            let concrete_storage = Arc::new(Storage::new(
                self.database.clone(),
                &(Arc::new(self.clock.clone()) as Arc<dyn Clock>),
            ));
            let delivery_storage: Arc<dyn kapsel_delivery::storage::DeliveryStorage> =
                Arc::new(PostgresDeliveryStorage::new(concrete_storage));
            let new_engine = DeliveryEngine::with_event_handler(
                delivery_storage,
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

    /// Create delivery engine on demand for tests that need it.
    ///
    /// This avoids the performance penalty of creating delivery engines
    /// for tests that don't need delivery functionality.
    pub fn create_delivery_engine(&mut self) -> anyhow::Result<()> {
        if self.delivery_engine.is_some() {
            return Ok(()); // Already exists
        }

        let delivery_config = DeliveryConfig {
            worker_count: 1,
            batch_size: 10,
            poll_interval: Duration::from_millis(100),
            shutdown_timeout: Duration::from_secs(1),
            client_config: ClientConfig {
                timeout: Duration::from_secs(2),
                user_agent: "Kapsel-Test/1.0".to_string(),
                max_redirects: 2,
                verify_tls: false,
            },
            default_retry_policy: RetryPolicy {
                max_attempts: 2,
                base_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(5),
                jitter_factor: 0.0,
                backoff_strategy: BackoffStrategy::Exponential,
            },
        };

        let clock_arc: Arc<dyn Clock> = Arc::new(self.clock.clone());
        let event_handler = Arc::new(NoOpEventHandler) as Arc<dyn EventHandler>;

        let concrete_storage = Arc::new(Storage::new(self.database.clone(), &clock_arc));
        let delivery_storage: Arc<dyn DeliveryStorage> =
            Arc::new(PostgresDeliveryStorage::new(concrete_storage));
        let engine = DeliveryEngine::with_event_handler(
            delivery_storage,
            delivery_config,
            clock_arc,
            event_handler,
        )?;

        self.delivery_engine = Some(engine);
        Ok(())
    }

    /// Runs a test function within a fully isolated, temporary database.
    ///
    /// This function GUARANTEES cleanup by:
    /// 1. Creating a new database from a template.
    /// 2. Constructing a TestEnv pointing to this new database.
    /// 3. Running the provided async test closure with the new TestEnv.
    /// 4. Dropping the temporary database AFTER the test closure completes,
    ///    regardless of whether it passed or failed.
    ///
    /// This is the recommended pattern for all tests that require a committed
    /// state or background workers (`new_isolated` use cases).
    pub async fn run_isolated_test<F, Fut>(test_fn: F) -> Result<()>
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let start_time = Instant::now();

        database::ensure_template_database_exists();

        let admin_pool_start = Instant::now();
        let admin_pool =
            database::create_admin_pool().await.context("failed to create admin pool")?;
        let admin_pool_duration = admin_pool_start.elapsed();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let db_name = format!("test_{}_{}", timestamp, Uuid::new_v4().simple());

        let db_create_start = Instant::now();
        database::create_database_from_template(&admin_pool, &db_name)
            .await
            .with_context(|| format!("failed to create isolated database: {db_name}"))?;
        let db_create_duration = db_create_start.elapsed();

        let env_build_start = Instant::now();
        let env = Self::build_for_isolated_db(&db_name)
            .await
            .with_context(|| format!("failed to build TestEnv for database: {db_name}"))?;
        let env_build_duration = env_build_start.elapsed();

        let setup_duration = start_time.elapsed();
        if setup_duration > Duration::from_millis(1000) {
            warn!(
                "Isolated test setup took {}ms (admin pool: {}ms, DB create: {}ms, env build: {}ms) for {}",
                setup_duration.as_millis(),
                admin_pool_duration.as_millis(),
                db_create_duration.as_millis(),
                env_build_duration.as_millis(),
                db_name
            );
        }

        let test_start = Instant::now();
        let test_result = std::panic::AssertUnwindSafe(test_fn(env)).catch_unwind().await;
        let test_duration = test_start.elapsed();

        let cleanup_start = Instant::now();
        match database::drop_database_immediate(&admin_pool, &db_name).await {
            Ok(()) => {
                debug!(database = %db_name, "Successfully cleaned up isolated test database");
            },
            Err(e) => {
                let error_msg = e.to_string().to_lowercase();
                if error_msg.contains("does not exist") || error_msg.contains("not exist") {
                    debug!(database = %db_name, "Database already cleaned up (container restart?)");
                } else {
                    warn!(database = %db_name, error = %e, "Failed to clean up isolated test database");

                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{db_name}\""))
                            .execute(&admin_pool),
                    )
                    .await;
                }
            },
        }
        let cleanup_duration = cleanup_start.elapsed();

        let total_duration = start_time.elapsed();
        info!(
            "Isolated test {} completed in {}ms (setup: {}ms, test: {}ms, cleanup: {}ms)",
            db_name,
            total_duration.as_millis(),
            setup_duration.as_millis(),
            test_duration.as_millis(),
            cleanup_duration.as_millis()
        );

        match test_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    /// Build TestEnv for a specific isolated database.
    async fn build_for_isolated_db(database_name: &str) -> Result<Self> {
        let start_time = Instant::now();

        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let pool_start = Instant::now();
        let database = database::create_database_pool(database_name)
            .await
            .with_context(|| format!("failed to create pool for database: {database_name}"))?;
        let pool_duration = pool_start.elapsed();

        let test_run_id = Uuid::new_v4().simple().to_string();

        let http_start = Instant::now();
        let http_mock = http::MockServer::start().await;
        let http_duration = http_start.elapsed();

        let clock = time::TestClock::new();
        let clock_arc: Arc<dyn Clock> = Arc::new(clock.clone());
        let storage = Arc::new(Storage::new(database.clone(), &clock_arc));

        let delivery_engine = None;

        let total_duration = start_time.elapsed();

        debug!(
            "build_for_isolated_db({}) took {}ms (pool: {}ms, HTTP: {}ms)",
            database_name,
            total_duration.as_millis(),
            pool_duration.as_millis(),
            http_duration.as_millis()
        );

        Ok(Self {
            http_mock,
            clock,
            database,
            storage,
            attestation_service: None,
            test_run_id,
            is_isolated: true,
            delivery_engine,
        })
    }
}
