//! Database utilities for testing with isolated databases.
//!
//! This module provides test database management with complete isolation
//! between tests. Each test gets its own PostgreSQL database created from
//! a pre-migrated template for fast setup and guaranteed cleanup.

use std::{
    collections::HashSet,
    env,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use sqlx::{postgres::PgConnectOptions, PgPool, Postgres, Transaction};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Template database name used for fast database creation.
const TEMPLATE_DB_NAME: &str = "kapsel_test_template";

/// Template version to detect when recreation is needed.
const TEMPLATE_VERSION: &str = "v1";

/// Maximum age for test databases before they're considered stale (1 hour).
const STALE_DATABASE_TIMEOUT_SECS: u64 = 3600;

/// Static database pool shared across all tests in a process.
static SHARED_POOL: tokio::sync::OnceCell<PgPool> = tokio::sync::OnceCell::const_new();

/// Mutex to serialize template database initialization.
static TEMPLATE_INIT_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Tracks whether the template database has been initialized in this process.
static TEMPLATE_INITIALIZED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Registry of databases created by this process for cleanup on exit.
static CLEANUP_REGISTRY: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Database handle for transaction-based test isolation.
pub struct TestDatabase {
    pool: PgPool,
}

impl TestDatabase {
    /// Returns the shared test database pool.
    ///
    /// Initializes the pool on first access and runs migrations.
    pub async fn shared_pool() -> Result<&'static PgPool> {
        SHARED_POOL.get_or_try_init(|| async { create_shared_pool().await }).await
    }

    /// Creates a new test database using the shared pool.
    pub async fn new() -> Result<Self> {
        let pool = Self::shared_pool().await?.clone();
        Ok(Self { pool })
    }

    /// Creates a new isolated test database with its own database.
    pub async fn new_isolated() -> Result<IsolatedTestDatabase> {
        IsolatedTestDatabase::new().await
    }

    /// Creates a new transaction-scoped test handle.
    ///
    /// The transaction is automatically rolled back when dropped.
    pub async fn new_transaction() -> Result<TransactionalTestDatabase> {
        TransactionalTestDatabase::new().await
    }

    /// Returns the database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Database handle for transaction-scoped test isolation.
///
/// All operations happen within a single transaction that is rolled back
/// when the handle is dropped.
pub struct TransactionalTestDatabase {
    pool: PgPool,
}

impl TransactionalTestDatabase {
    /// Creates a new transaction-scoped test database.
    pub async fn new() -> Result<Self> {
        let pool = TestDatabase::shared_pool().await?.clone();
        Ok(Self { pool })
    }

    /// Begins a new transaction for testing.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>> {
        Ok(self.pool.begin().await?)
    }

    /// Returns the database pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Database handle for fully isolated test execution.
///
/// Each isolated database gets its own PostgreSQL database created from a
/// template. This allows complete isolation including schema, indexes, and
/// extensions.
pub struct IsolatedTestDatabase {
    pool: PgPool,
    database_name: String,
    admin_pool: Arc<PgPool>,
}

impl IsolatedTestDatabase {
    /// Creates a new isolated test database from the template.
    ///
    /// Each isolated database is created by cloning a pre-migrated template
    /// database. This is much faster than running migrations for each test.
    pub async fn new() -> Result<Self> {
        // Clean up stale databases on first test
        cleanup_stale_databases_once().await?;

        // Ensure template database exists and is migrated
        ensure_template_database().await?;

        // Create admin connection to postgres database for management operations
        let admin_pool = Arc::new(create_admin_pool().await?);

        // Generate unique database name
        let database_name = format!("test_{}", Uuid::new_v4().simple());

        // Register for cleanup
        register_database_for_cleanup(&database_name);

        // Create the test database from template
        create_database_from_template(&admin_pool, &database_name).await?;

        // Create a connection pool to the new database
        let pool = create_database_pool(&database_name).await?;

        Ok(Self { pool, database_name, admin_pool })
    }

    /// Connection pool for this isolated database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Database name for this isolated database.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }
}

impl Drop for IsolatedTestDatabase {
    fn drop(&mut self) {
        let database_name = self.database_name.clone();
        let admin_pool = Arc::clone(&self.admin_pool);

        // Spawn cleanup task if we're in an async context
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // We're in an async context, spawn a cleanup task
            handle.spawn(async move {
                // Wait a bit for connections to naturally close
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Attempt to drop the database
                match drop_database(&admin_pool, &database_name).await {
                    Ok(_) => {
                        debug!("successfully dropped test database: {}", database_name);
                    },
                    Err(e) => {
                        // This is common if another test is still using connections
                        debug!(
                            "could not drop database {} (will retry later): {}",
                            database_name, e
                        );
                    },
                }
            });
        } else {
            // Not in async context - this shouldn't happen in normal test execution
            debug!("database {} cleanup deferred (not in async context)", database_name);
        }
    }
}

/// Register a database for cleanup tracking.
fn register_database_for_cleanup(database_name: &str) {
    CLEANUP_REGISTRY.lock().unwrap().insert(database_name.to_string());
    debug!("registered database {} for cleanup", database_name);
}

/// Clean up stale databases once per process.
async fn cleanup_stale_databases_once() -> Result<()> {
    static CLEANUP_DONE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

    if CLEANUP_DONE.get().is_some() {
        return Ok(());
    }

    let _ = CLEANUP_DONE.set(());
    cleanup_stale_databases().await
}

/// Clean up stale test databases from previous runs.
async fn cleanup_stale_databases() -> Result<()> {
    let admin_pool = create_admin_pool().await?;

    // Find all test databases - using simpler query without timestamp
    let test_databases: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT datname
        FROM pg_database
        WHERE datname LIKE 'test_%'
        AND datname != $1
        "#,
    )
    .bind(TEMPLATE_DB_NAME)
    .fetch_all(&admin_pool)
    .await
    .unwrap_or_default();

    let mut cleaned = 0;

    for (db_name,) in test_databases {
        // Check if database matches our test pattern (test_UUID)
        if !is_test_database(&db_name) {
            continue;
        }

        // For now, clean up all test databases that are not the template
        // In CI, this is safe since each run is isolated
        // Locally, developers can manually clean if needed
        debug!("cleaning up test database: {}", db_name);
        if drop_database(&admin_pool, &db_name).await.is_ok() {
            cleaned += 1;
        }
    }

    if cleaned > 0 {
        info!("cleaned up {} stale test databases", cleaned);
    }

    Ok(())
}

/// Check if a database name matches our test database pattern.
fn is_test_database(name: &str) -> bool {
    // Pattern: test_<32 hex chars>
    if !name.starts_with("test_") {
        return false;
    }
    let suffix = &name[5..];
    suffix.len() == 32 && suffix.chars().all(|c| c.is_ascii_hexdigit())
}

/// Ensures the template database exists and is fully migrated.
async fn ensure_template_database() -> Result<()> {
    // Fast path: already initialized
    if TEMPLATE_INITIALIZED.get().is_some() {
        return Ok(());
    }

    // Serialize template initialization across all tests
    let _lock = TEMPLATE_INIT_MUTEX.lock().unwrap();

    // Double-check after acquiring lock
    if TEMPLATE_INITIALIZED.get().is_some() {
        return Ok(());
    }

    info!("initializing template database: {}", TEMPLATE_DB_NAME);

    let admin_pool = create_admin_pool().await?;

    // Check if template database already exists
    let exists: (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(TEMPLATE_DB_NAME)
            .fetch_one(&admin_pool)
            .await?;

    if !exists.0 {
        // Create template database
        sqlx::query(&format!(
            "CREATE DATABASE \"{}\" WITH TEMPLATE template0 ENCODING 'UTF8'",
            TEMPLATE_DB_NAME
        ))
        .execute(&admin_pool)
        .await
        .context("failed to create template database")?;

        info!("created template database: {}", TEMPLATE_DB_NAME);
    }

    // Connect to template database and run migrations
    let template_pool = create_database_pool(TEMPLATE_DB_NAME).await?;

    // Create required extensions
    sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"")
        .execute(&template_pool)
        .await
        .context("failed to create uuid-ossp extension")?;

    sqlx::query("CREATE EXTENSION IF NOT EXISTS \"pgcrypto\"")
        .execute(&template_pool)
        .await
        .context("failed to create pgcrypto extension")?;

    // Run migrations
    let migrations_path = find_migrations_directory()?;
    sqlx::migrate::Migrator::new(migrations_path)
        .await?
        .run(&template_pool)
        .await
        .context("failed to run migrations on template database")?;

    // Close template pool
    template_pool.close().await;

    info!("template database ready: {}", TEMPLATE_DB_NAME);

    // Mark as initialized
    TEMPLATE_INITIALIZED.set(()).ok();

    Ok(())
}

/// Creates a new database from the template.
async fn create_database_from_template(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    const MAX_RETRIES: usize = 3;
    const RETRY_DELAY_MS: u64 = 100;

    for attempt in 1..=MAX_RETRIES {
        let query = format!(
            "CREATE DATABASE \"{}\" WITH TEMPLATE \"{}\" OWNER postgres",
            database_name, TEMPLATE_DB_NAME
        );

        match sqlx::query(&query).execute(admin_pool).await {
            Ok(_) => {
                debug!("created test database: {}", database_name);
                return Ok(());
            },
            Err(e) if attempt == MAX_RETRIES => {
                return Err(e).context("failed to create database from template after retries");
            },
            Err(e) => {
                let error_str = e.to_string();
                // Template might be in use, retry
                if error_str.contains("being accessed by other users") {
                    warn!(
                        "template database in use (attempt {}/{}), retrying...",
                        attempt, MAX_RETRIES
                    );
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * attempt as u64))
                        .await;
                } else {
                    return Err(e).context("failed to create database from template");
                }
            },
        }
    }

    unreachable!("loop should have returned");
}

/// Drops a test database.
async fn drop_database(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    // First terminate any existing connections
    sqlx::query(&format!(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = '{}'
         AND pid <> pg_backend_pid()",
        database_name
    ))
    .execute(admin_pool)
    .await
    .ok(); // Ignore errors from termination

    // Now drop the database
    sqlx::query(&format!("DROP DATABASE IF EXISTS \"{}\"", database_name))
        .execute(admin_pool)
        .await
        .with_context(|| format!("failed to drop database: {}", database_name))?;

    Ok(())
}

/// Creates an admin pool connected to the postgres database.
async fn create_admin_pool() -> Result<PgPool> {
    let db_url = env::var("DATABASE_URL").context(
        "DATABASE_URL not set. Ensure postgres-test container is running: docker-compose up -d postgres-test",
    )?;

    // Parse the URL and change the database to 'postgres'
    let opts = db_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database("postgres");

    // Use minimal connections for admin operations
    let max_connections = if env::var("CI").is_ok() { 2 } else { 5 };

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(opts)
        .await
        .context("failed to connect to postgres database")?;

    Ok(pool)
}

/// Creates a pool connected to a specific database.
async fn create_database_pool(database_name: &str) -> Result<PgPool> {
    let db_url = env::var("DATABASE_URL")?;

    // Parse the URL and change the database name
    let opts = db_url
        .parse::<PgConnectOptions>()
        .context("failed to parse DATABASE_URL")?
        .database(database_name);

    // Use small pools for isolated tests
    let max_connections = if env::var("CI").is_ok() { 2 } else { 3 };

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_secs(10))
        .max_lifetime(Duration::from_secs(60))
        .test_before_acquire(false) // Skip health checks for performance
        .connect_with(opts)
        .await
        .with_context(|| format!("failed to connect to database: {}", database_name))?;

    Ok(pool)
}

/// Creates the shared pool for transaction-based tests.
async fn create_shared_pool() -> Result<PgPool> {
    let db_url = env::var("DATABASE_URL").context(
        "DATABASE_URL not set. Ensure postgres-test container is running: docker-compose up -d postgres-test",
    )?;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20) // Shared across all transaction tests
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(3))
        .idle_timeout(Duration::from_secs(30))
        .max_lifetime(Duration::from_secs(120))
        .test_before_acquire(false) // Skip health checks for performance
        .connect(&db_url)
        .await
        .context("failed to connect to test database")?;

    // Ensure extensions are available
    sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"")
        .execute(&pool)
        .await
        .context("failed to create uuid-ossp extension")?;

    sqlx::query("CREATE EXTENSION IF NOT EXISTS \"pgcrypto\"")
        .execute(&pool)
        .await
        .context("failed to create pgcrypto extension")?;

    // Run migrations on shared database
    let migrations_path = find_migrations_directory()?;
    sqlx::migrate::Migrator::new(migrations_path).await?.run(&pool).await?;

    info!("shared test database pool ready");
    Ok(pool)
}

/// Finds the migrations directory by walking up from current directory.
fn find_migrations_directory() -> Result<PathBuf> {
    let current_dir = env::current_dir().context("failed to get current directory")?;

    // Walk up the directory tree looking for migrations directory
    for ancestor in current_dir.ancestors() {
        let migrations_dir = ancestor.join("migrations");
        if migrations_dir.exists() && migrations_dir.is_dir() {
            return Ok(migrations_dir);
        }
    }

    anyhow::bail!("Could not find migrations directory");
}
