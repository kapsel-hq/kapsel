//! Database testing infrastructure with automatic container management.
//!
//! Implements template database cloning for fast test isolation. A file-lock
//! protected singleton ensures exactly one container per test run while
//! providing each test with an isolated database clone.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use sqlx::{Connection, Executor, PgConnection, PgPool};
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use tokio::sync::OnceCell;
use tracing::{info, warn};
use uuid::Uuid;

/// Global container state with reference counting for proper cleanup.
static CONTAINER_STATE: OnceCell<Arc<ContainerState>> = OnceCell::const_new();

/// Tracks the shared container and ensures cleanup when no longer needed.
struct ContainerState {
    maintenance_url: String,
    template_name: String,
    reference_count: AtomicUsize,
    info_file_path: PathBuf,
    role: ContainerRole,
}

/// Distinguishes between process that owns the container and follower
/// processes.
enum ContainerRole {
    Leader(#[allow(dead_code)] Box<ContainerAsync<PostgresImage>>),
    Follower,
}

/// Database connection information shared between processes.
#[derive(serde::Serialize, serde::Deserialize)]
struct DatabaseInfo {
    maintenance_url: String,
    template_name: String,
}

/// Isolated database handle for a single test.
pub struct TestDatabase {
    pool: PgPool,
    db_name: String,
    maintenance_url: String,
    container_state: Arc<ContainerState>,
}

impl TestDatabase {
    /// Creates a new isolated database by cloning the pre-migrated template.
    pub async fn new() -> Result<Self> {
        let container_state = get_or_create_container().await?;
        container_state.reference_count.fetch_add(1, Ordering::SeqCst);

        let db_name = generate_database_name();

        let mut conn = connect_maintenance_database(&container_state.maintenance_url).await?;

        create_database_from_template(&mut conn, &db_name, &container_state.template_name).await?;

        let pool = connect_test_database(&container_state.maintenance_url, &db_name).await?;

        info!(database = %db_name, "Created isolated test database");

        Ok(Self {
            pool,
            db_name,
            maintenance_url: container_state.maintenance_url.clone(),
            container_state,
        })
    }

    /// Returns the connection pool for this database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let url = self.maintenance_url.clone();
        let name = self.db_name.clone();
        let container_state = Arc::clone(&self.container_state);

        // Spawn cleanup task with proper error handling
        tokio::spawn(async move {
            cleanup_test_database(&url, &name).await;

            let refs = container_state.reference_count.fetch_sub(1, Ordering::SeqCst);
            if refs == 1 {
                container_state.cleanup();
            }
        });
    }
}

impl ContainerState {
    /// Creates container state with file lock coordination across processes.
    async fn with_lock_coordination() -> Result<Arc<Self>> {
        let target_dir = find_target_directory()?;
        let lock_path = target_dir.join("kapsel_test.lock");
        let info_path = target_dir.join("kapsel_database.json");

        fs::create_dir_all(&target_dir)?;

        let mut lock = fslock::LockFile::open(&lock_path)?;
        lock.lock()?;

        // Check if container is already running from another process
        if info_path.exists() {
            if let Ok(info) = read_database_info(&info_path) {
                if validate_container_running(&info.maintenance_url).await {
                    lock.unlock()?;
                    info!("Using existing container at {}", info.maintenance_url);
                    return Ok(Self::create_follower(info, info_path));
                }
            }
            // Container is dead, remove stale info
            let _ = fs::remove_file(&info_path);
        }

        // Create new container as leader
        let container_state = Self::create_leader(&info_path).await?;

        write_database_info(&info_path, &container_state)?;

        lock.unlock()?;
        info!("Container and template database ready");

        Ok(container_state)
    }

    /// Creates leader container state that owns the PostgreSQL container.
    async fn create_leader(info_path: &Path) -> Result<Arc<Self>> {
        info!("Initializing PostgreSQL container");

        let container = AsyncRunner::start(
            PostgresImage::default()
                .with_tag("16-alpine")
                .with_env_var("POSTGRES_INITDB_ARGS", "--data-checksums")
                .with_env_var("PGDATA", "/var/lib/postgresql/data/pgdata"),
        )
        .await?;

        let port = container.get_host_port_ipv4(5432).await?;
        let maintenance_url =
            format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable");

        let mut conn = PgConnection::connect(&maintenance_url).await?;

        let template_name = "kapsel_template";
        drop_database_if_exists(&mut conn, template_name).await?;
        create_database(&mut conn, template_name).await?;

        let template_url = build_database_url(&maintenance_url, template_name)?;

        // Configure PostgreSQL for minimal disk usage
        configure_postgres_for_testing(&mut conn).await?;

        run_migrations(&template_url).await?;

        Ok(Arc::new(Self {
            maintenance_url,
            template_name: template_name.to_string(),
            reference_count: AtomicUsize::new(0),
            info_file_path: info_path.to_path_buf(),
            role: ContainerRole::Leader(Box::new(container)),
        }))
    }

    /// Creates follower container state that connects to existing container.
    fn create_follower(info: DatabaseInfo, info_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            maintenance_url: info.maintenance_url,
            template_name: info.template_name,
            reference_count: AtomicUsize::new(0),
            info_file_path: info_path,
            role: ContainerRole::Follower,
        })
    }

    /// Cleans up container state and removes coordination files.
    fn cleanup(&self) {
        info!("Cleaning up container state");

        // Remove the info file to signal other processes
        let _ = fs::remove_file(&self.info_file_path);

        // Container cleanup is automatic via Drop trait on ContainerAsync
        match &self.role {
            ContainerRole::Leader(_) => {
                info!("Leader process cleaning up container");
            },
            ContainerRole::Follower => {
                info!("Follower process finished");
            },
        }
    }
}

async fn get_or_create_container() -> Result<Arc<ContainerState>> {
    CONTAINER_STATE
        .get_or_try_init(|| async { ContainerState::with_lock_coordination().await })
        .await
        .map(Arc::clone)
}

async fn run_migrations(database_url: &str) -> Result<()> {
    let pool = PgPool::connect(database_url).await?;

    create_extensions(&pool).await?;

    let migrations_path = find_migrations_directory()?;
    sqlx::migrate::Migrator::new(migrations_path).await?.run(&pool).await?;

    pool.close().await;
    Ok(())
}

async fn create_extensions(pool: &PgPool) -> Result<()> {
    sqlx::query("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"").execute(pool).await?;
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pgcrypto").execute(pool).await?;
    Ok(())
}

async fn configure_postgres_for_testing(conn: &mut PgConnection) -> Result<()> {
    // Configure PostgreSQL for minimal disk usage during testing
    let config_queries = [
        // Reduce WAL usage
        "ALTER SYSTEM SET wal_level = 'minimal'",
        "ALTER SYSTEM SET max_wal_size = '16MB'",
        "ALTER SYSTEM SET min_wal_size = '8MB'",
        "ALTER SYSTEM SET checkpoint_completion_target = 0.9",
        "ALTER SYSTEM SET checkpoint_timeout = '30s'",
        // Reduce buffer usage but keep reasonable for tests
        "ALTER SYSTEM SET shared_buffers = '32MB'",
        "ALTER SYSTEM SET work_mem = '4MB'",
        // Disable expensive safety features for testing (unsafe for production!)
        "ALTER SYSTEM SET fsync = off",
        "ALTER SYSTEM SET synchronous_commit = off",
        "ALTER SYSTEM SET full_page_writes = off",
        // Reduce logging
        "ALTER SYSTEM SET log_statement = 'none'",
        "ALTER SYSTEM SET log_duration = off",
        "ALTER SYSTEM SET log_min_duration_statement = -1",
        // Auto-vacuum more aggressively to prevent bloat
        "ALTER SYSTEM SET autovacuum_naptime = '10s'",
        "ALTER SYSTEM SET autovacuum_vacuum_scale_factor = 0.1",
        "ALTER SYSTEM SET autovacuum_analyze_scale_factor = 0.05",
    ];

    for query in config_queries {
        if let Err(e) = conn.execute(query).await {
            warn!(error = %e, query = %query, "Failed to set PostgreSQL configuration");
        }
    }

    // Reload configuration
    if let Err(e) = conn.execute("SELECT pg_reload_conf()").await {
        warn!(error = %e, "Failed to reload PostgreSQL configuration");
    }

    Ok(())
}

fn find_target_directory() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir));
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(|p| p.join("target"))
        .ok_or_else(|| anyhow::anyhow!("Could not find workspace target directory"))
}

fn find_migrations_directory() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(|p| p.join("migrations"))
        .ok_or_else(|| anyhow::anyhow!("Could not find migrations directory"))
}

async fn validate_container_running(maintenance_url: &str) -> bool {
    match PgConnection::connect(maintenance_url).await {
        Ok(mut conn) => (sqlx::query("SELECT 1").execute(&mut conn).await).is_ok(),
        Err(_) => false,
    }
}

fn read_database_info(path: &Path) -> Result<DatabaseInfo> {
    let json = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&json)?)
}

fn write_database_info(path: &Path, container_state: &ContainerState) -> Result<()> {
    let info = DatabaseInfo {
        maintenance_url: container_state.maintenance_url.clone(),
        template_name: container_state.template_name.clone(),
    };
    fs::write(path, serde_json::to_string(&info)?)?;
    Ok(())
}

fn generate_database_name() -> String {
    format!("kapsel_test_{}", Uuid::new_v4().simple())
}

async fn connect_maintenance_database(maintenance_url: &str) -> Result<PgConnection> {
    PgConnection::connect(maintenance_url)
        .await
        .context("Failed to connect to maintenance database")
}

async fn create_database_from_template(
    conn: &mut PgConnection,
    db_name: &str,
    template_name: &str,
) -> Result<()> {
    let query = format!(r#"CREATE DATABASE "{db_name}" WITH TEMPLATE "{template_name}""#);
    conn.execute(query.as_str()).await.context("Failed to create test database from template")?;
    Ok(())
}

async fn connect_test_database(maintenance_url: &str, db_name: &str) -> Result<PgPool> {
    let mut db_url = url::Url::parse(maintenance_url)?;
    db_url.set_path(db_name);

    PgPool::connect(db_url.as_str()).await.context("Failed to connect to test database")
}

async fn cleanup_test_database(url: &str, name: &str) {
    if let Ok(mut conn) = PgConnection::connect(url).await {
        // Terminate connections to the database before dropping
        let terminate_query = format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{name}' AND pid <> pg_backend_pid()"
        );
        let _ = conn.execute(terminate_query.as_str()).await;

        // Drop the database with force
        let query = format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#);
        if let Err(e) = conn.execute(query.as_str()).await {
            warn!(error = %e, database = %name, "Failed to drop test database");
        }

        // Force checkpoint and cleanup after dropping database
        let _ = conn.execute("CHECKPOINT").await;

        // Clean up any remaining temporary files
        let _ = conn.execute("SELECT pg_stat_reset()").await;
    }
}

async fn drop_database_if_exists(conn: &mut PgConnection, name: &str) -> Result<()> {
    let query = format!(r#"DROP DATABASE IF EXISTS "{name}""#);
    conn.execute(query.as_str()).await?;
    Ok(())
}

async fn create_database(conn: &mut PgConnection, name: &str) -> Result<()> {
    let query = format!(r#"CREATE DATABASE "{name}""#);
    conn.execute(query.as_str()).await?;
    Ok(())
}

fn build_database_url(maintenance_url: &str, database_name: &str) -> Result<String> {
    let mut template_url = url::Url::parse(maintenance_url)?;
    template_url.set_path(database_name);
    Ok(template_url.to_string())
}

/// Legacy compatibility for SharedDatabase.
pub struct SharedDatabase {
    pool: PgPool,
}

impl SharedDatabase {
    /// Returns the connection pool for this shared database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Creates a connection to the shared database instance.
pub async fn create_shared_database() -> Result<std::sync::Arc<SharedDatabase>> {
    let test_db = TestDatabase::new().await?;
    Ok(std::sync::Arc::new(SharedDatabase { pool: test_db.pool.clone() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn isolated_databases_have_separate_data() {
        let db1 = TestDatabase::new().await.unwrap();
        let db2 = TestDatabase::new().await.unwrap();

        sqlx::query("CREATE TABLE test_isolation (id INT)").execute(db1.pool()).await.unwrap();

        let result = sqlx::query("SELECT 1 FROM test_isolation").fetch_optional(db2.pool()).await;

        assert!(result.is_err(), "Tables should be isolated between databases");
    }

    #[tokio::test]
    async fn database_cleanup_on_drop() {
        let db_name = {
            let db = TestDatabase::new().await.unwrap();
            db.db_name.clone()
        };

        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        let container_state = CONTAINER_STATE.get().unwrap();
        let mut conn = PgConnection::connect(&container_state.maintenance_url).await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pg_database WHERE datname = $1")
            .bind(&db_name)
            .fetch_one(&mut conn)
            .await
            .unwrap();

        assert_eq!(count.0, 0, "Database should be cleaned up after drop");
    }
}
