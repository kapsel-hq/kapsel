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
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const TEMPLATE_DB_NAME: &str = "kapsel_test_template";
const MAIN_TEST_DB_NAME: &str = "kapsel_test";

// Cleanup databases older than this many seconds
const CLEANUP_AGE_THRESHOLD_SECS: i64 = 30;

// Thread-local shared pool for transaction-based tests
// Each test runtime gets its own pool to avoid cross-runtime contamination
thread_local! {
    static SHARED_POOL: std::cell::RefCell<Option<PgPool>> = std::cell::RefCell::new(None);
}
static TEMPLATE_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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

/// Clean up old test databases based on timestamp in name.
async fn cleanup_old_test_databases(admin_pool: &PgPool) -> Result<()> {
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX);

    // Find all test databases with timestamps
    let databases: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database
         WHERE datname LIKE 'test_%_%'
         AND datname != $1",
    )
    .bind(TEMPLATE_DB_NAME)
    .fetch_all(admin_pool)
    .await
    .context("failed to list test databases")?;

    let mut cleaned = 0;
    let mut skipped = 0;

    for db_name in databases {
        // Extract timestamp from database name: test_<timestamp>_<uuid>
        if let Some(timestamp_str) = extract_timestamp_from_db_name(&db_name) {
            if let Ok(db_timestamp) = timestamp_str.parse::<i64>() {
                let age_seconds = current_time - db_timestamp;

                if age_seconds > CLEANUP_AGE_THRESHOLD_SECS {
                    // Database is old enough to clean up
                    match drop_database_immediate(admin_pool, &db_name).await {
                        Ok(()) => {
                            debug!("cleaned up old database: {} (age: {}s)", db_name, age_seconds);
                            cleaned += 1;
                        },
                        Err(e) => {
                            debug!("failed to cleanup database {}: {}", db_name, e);
                        },
                    }
                } else {
                    skipped += 1;
                }
            }
        }
    }

    if cleaned > 0 || skipped > 0 {
        info!(
            "cleanup complete: {} databases cleaned, {} recent databases skipped",
            cleaned, skipped
        );
    }

    Ok(())
}

/// Extract timestamp from database name format: test_<timestamp>_<uuid>
fn extract_timestamp_from_db_name(db_name: &str) -> Option<&str> {
    if !db_name.starts_with("test_") {
        return None;
    }

    let parts: Vec<&str> = db_name.splitn(3, '_').collect();
    if parts.len() == 3 && parts[0] == "test" {
        Some(parts[1])
    } else {
        None
    }
}

// Database creation functions

/// Create database from template.
async fn create_database_from_template(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    const MAX_RETRIES: usize = 5;
    const RETRY_DELAY_MS: u64 = 100;

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
                    let error_msg = last_error.as_ref().map_or_else(
                        || "unknown error".to_string(),
                        std::string::ToString::to_string,
                    );
                    warn!(
                        "failed to create database {} (attempt {}/{}): {} - retrying in {}ms",
                        database_name, attempt, MAX_RETRIES, error_msg, RETRY_DELAY_MS
                    );
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                } else {
                    let error_msg = last_error.as_ref().map_or_else(
                        || "unknown error".to_string(),
                        std::string::ToString::to_string,
                    );
                    error!(
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

/// Drop database immediately with connection termination.
async fn drop_database_immediate(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    // Terminate any existing connections
    let _ = sqlx::query(&format!(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = '{database_name}'
         AND pid <> pg_backend_pid()"
    ))
    .execute(admin_pool)
    .await;

    // Brief delay for terminations to complete
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Drop the database
    sqlx::query(&format!("DROP DATABASE IF EXISTS \"{database_name}\""))
        .execute(admin_pool)
        .await
        .with_context(|| format!("failed to drop database: {database_name}"))?;

    Ok(())
}

// Connection pool creation functions

/// Create admin connection pool for database management operations.
async fn create_admin_pool() -> Result<PgPool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

    let opts = database_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database("postgres");

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(60))
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(opts)
        .await
        .context("failed to create admin connection pool")?;

    Ok(pool)
}

/// Create connection pool for specific database.
async fn create_database_pool(database_name: &str) -> Result<PgPool> {
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
        .max_connections(15)  // Higher limit for concurrent test execution and property tests
        .min_connections(0)   // No persistent connections to avoid shutdown issues
        .max_lifetime(Duration::from_secs(300))  // Longer lifetime for stability
        .idle_timeout(Duration::from_secs(60))   // Keep connections alive longer
        .acquire_timeout(Duration::from_secs(60)) // More time for connection contention in concurrent tests
        .connect_with(opts)
        .await
        .context("failed to create shared connection pool")?;

    Ok(pool)
}

/// Close the shared connection pool to prevent Tokio shutdown errors.
/// This should be called during test cleanup when possible.
pub async fn close_shared_pool() {
    SHARED_POOL.with(|pool_cell| {
        let mut pool_ref = pool_cell.borrow_mut();
        if let Some(pool) = pool_ref.take() {
            tokio::spawn(async move {
                pool.close().await;
            });
        }
    });
}

/// Clean up any orphaned databases manually (for debugging).
pub async fn cleanup_orphaned_databases() -> Result<()> {
    let admin_pool = create_admin_pool().await?;
    cleanup_old_test_databases(&admin_pool).await
}
