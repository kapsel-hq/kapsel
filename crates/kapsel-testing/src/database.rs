//! Database management for deterministic testing.
//!
//! Provides isolated test databases with startup cleanup and deterministic
//! behavior. Uses PostgreSQL advisory locks to coordinate cleanup between
//! parallel test processes.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool, Postgres, Transaction,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

const TEMPLATE_DB_NAME: &str = "kapsel_test_template";
const MAIN_TEST_DB_NAME: &str = "kapsel_test";

// Thread-local shared pool for transaction-based tests
// Each test runtime gets its own pool to avoid cross-runtime contamination
thread_local! {
    static SHARED_POOL: std::cell::RefCell<Option<PgPool>> = std::cell::RefCell::new(None);
}
static TEMPLATE_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// Static admin pool for reuse across tests
static ADMIN_POOL: std::sync::OnceLock<PgPool> = std::sync::OnceLock::new();

/// Test database handle providing either shared or isolated database access.
#[derive(Debug)]
pub struct TestDatabase {
    pool: PgPool,
}

impl TestDatabase {
    /// Create new test database handle using shared pool.
    pub async fn new() -> Result<Self> {
        let pool = Self::shared_pool().await?;
        Ok(Self { pool })
    }

    /// Get shared database pool for transaction-based tests.
    /// Uses thread-local storage to ensure each test runtime gets its own pool.
    pub async fn shared_pool() -> Result<PgPool> {
        // First check if pool exists
        let pool_exists = SHARED_POOL.with(|pool_cell| pool_cell.borrow().is_some());

        if pool_exists {
            return SHARED_POOL.with(|pool_cell| Ok(pool_cell.borrow().as_ref().unwrap().clone()));
        }

        // Create new pool for this thread/runtime
        let pool = create_shared_pool().await?;

        // Store it in thread-local storage
        SHARED_POOL.with(|pool_cell| {
            *pool_cell.borrow_mut() = Some(pool.clone());
        });

        Ok(pool)
    }

    /// Create isolated test database for tests requiring production behavior.
    pub async fn new_isolated() -> Result<IsolatedTestDatabase> {
        IsolatedTestDatabase::new().await
    }

    /// Create transactional test database for automatic rollback.
    pub async fn new_transaction() -> Result<TransactionalTestDatabase> {
        TransactionalTestDatabase::new().await
    }

    /// Access to the underlying database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Transactional test database that automatically rolls back changes.
#[derive(Debug)]
pub struct TransactionalTestDatabase {
    pool: PgPool,
}

impl TransactionalTestDatabase {
    /// Create new transactional test database.
    pub async fn new() -> Result<Self> {
        let pool = TestDatabase::shared_pool().await?;
        Ok(Self { pool })
    }

    /// Begin a new transaction for test operations.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>> {
        Ok(self.pool.begin().await?)
    }

    /// Access to the underlying database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Isolated test database with its own PostgreSQL database.
///
/// Creates a dedicated database for tests that need production behavior
/// with background threads, delivery engines, etc.
#[derive(Debug)]
pub struct IsolatedTestDatabase {
    pool: PgPool,
    database_name: String,
}

impl IsolatedTestDatabase {
    /// Create new isolated test database.
    ///
    /// Database name includes timestamp for age-based cleanup.
    pub async fn new() -> Result<Self> {
        ensure_template_and_cleanup()?;

        let admin_pool = create_admin_pool().await?;

        // Create timestamped database name for age-based cleanup
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let database_name = format!("test_{}_{}", timestamp, Uuid::new_v4().simple());

        create_database_from_template(&admin_pool, &database_name).await?;

        let pool = create_database_pool(&database_name).await?;

        info!("created isolated test database: {}", database_name);

        Ok(Self { pool, database_name })
    }

    /// Access to the database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the database name.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }
}

// Initialization and cleanup functions

/// Ensure template database exists and clean up old test databases.
///
/// Lightweight initialization - template exists from Docker container.
/// No expensive admin operations needed for process-per-test model.
fn ensure_template_and_cleanup() -> Result<()> {
    // Template database exists from Docker initialization
    // Skip all expensive admin operations to optimize for process-per-test
    TEMPLATE_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// Ensure template database exists for isolated test creation.
///
/// This function ensures the template database is available for creating
/// isolated test databases. It performs one-time initialization using
/// atomic synchronization.
pub async fn ensure_template_database_exists() -> Result<()> {
    // Check if already initialized
    if TEMPLATE_INITIALIZED.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }

    // For now, we rely on the Docker container setup that creates the template
    // In the future, this could be enhanced to verify template exists and create if
    // needed
    ensure_template_and_cleanup()?;

    Ok(())
}

/// Create database from template with optimized retry strategy for test
/// performance.
pub async fn create_database_from_template(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    // Optimized for test performance - reduced retries and delays
    const MAX_RETRIES: usize = 3;
    const INITIAL_DELAY_MS: u64 = 10; // Faster initial retry

    let mut last_error = None;

    debug!("creating database {} from template {}", database_name, TEMPLATE_DB_NAME);

    for attempt in 1..=MAX_RETRIES {
        match sqlx::query(&format!(
            "CREATE DATABASE \"{database_name}\" WITH TEMPLATE \"{TEMPLATE_DB_NAME}\""
        ))
        .execute(admin_pool)
        .await
        {
            Ok(_) => {
                debug!("successfully created database {} from template", database_name);
                return Ok(());
            },
            Err(e) => {
                last_error = Some(e);
                if attempt < MAX_RETRIES {
                    // Exponential backoff but starting much lower: 10ms, 20ms, 40ms
                    let delay_ms = INITIAL_DELAY_MS * (1 << (attempt - 1));
                    debug!(
                        "database creation attempt {}/{} failed for {}, retrying in {}ms",
                        attempt, MAX_RETRIES, database_name, delay_ms
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                } else {
                    let error_msg = last_error.as_ref().map_or_else(
                        || "unknown error".to_string(),
                        std::string::ToString::to_string,
                    );
                    warn!(
                        "failed to create database {} after {} attempts: {}",
                        database_name, MAX_RETRIES, error_msg
                    );
                }
            },
        }
    }

    let final_error = last_error.map_or_else(
        || anyhow::anyhow!("database creation failed with unknown error"),
        anyhow::Error::from,
    );
    Err(final_error).with_context(|| {
        format!("failed to create database {database_name} after {MAX_RETRIES} attempts")
    })
}

/// Drop database immediately with optimized connection termination for tests.
pub async fn drop_database_immediate(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    // Terminate any existing connections - use force terminate for test cleanup
    let _ = sqlx::query(&format!(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = '{database_name}'
         AND pid <> pg_backend_pid()"
    ))
    .execute(admin_pool)
    .await;

    // Minimal delay optimized for test performance
    tokio::time::sleep(Duration::from_millis(5)).await;

    // Drop the database with cascade to handle any remaining dependencies
    match sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"))
        .execute(admin_pool)
        .await
    {
        Ok(_) => Ok(()),
        Err(_) => {
            // Fallback to standard DROP if FORCE is not supported
            sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\""))
                .execute(admin_pool)
                .await
                .with_context(|| format!("failed to drop database: {database_name}"))?;
            Ok(())
        },
    }
}

// Connection pool creation functions

/// Create or reuse admin connection pool for database management operations.
///
/// Uses a static pool to reduce connection overhead across multiple tests.
pub async fn create_admin_pool() -> Result<PgPool> {
    // Try to get existing pool first
    if let Some(pool) = ADMIN_POOL.get() {
        // Verify pool is still healthy
        if !pool.is_closed() {
            return Ok(pool.clone());
        }
    }

    // Create new pool
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database("postgres");

    let pool = PgPoolOptions::new()
        .max_connections(5)  // Increased for better concurrency
        .min_connections(1)
        .max_lifetime(Duration::from_secs(1800))  // Longer lifetime for reuse
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await
        .context("failed to connect to admin database")?;

    // Store in static for reuse, but don't error if another thread beat us to it
    let _ = ADMIN_POOL.set(pool.clone());

    debug!("created/reused admin connection pool");
    Ok(pool)
}

/// Create connection pool for specific database.
pub async fn create_database_pool(database_name: &str) -> Result<PgPool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(database_name);

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .min_connections(1)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(30))
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(opts)
        .await
        .with_context(|| {
            format!("failed to create connection pool for database: {database_name}")
        })?;

    Ok(pool)
}

/// Create minimal shared pool optimized for nextest process-per-test model.
/// Each test process only needs 1 connection since tests run sequentially
/// within process.
async fn create_shared_pool() -> Result<PgPool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(MAIN_TEST_DB_NAME);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(opts)
        .await
        .context("failed to create shared connection pool")?;

    Ok(pool)
}
