//! Database testing utilities.
//!
//! Provides isolated test databases with multiple backend support:
//! - SQLite: In-memory databases for fast unit tests (default)
//! - PostgreSQL: Full integration tests with testcontainers (requires Docker)

use anyhow::{Context, Result};
use sqlx::{postgres::PgConnectOptions, PgPool, Pool, Postgres, Sqlite};
use uuid::Uuid;

/// Database backend abstraction for tests.
pub enum TestDatabase {
    Sqlite { pool: Pool<Sqlite> },
    Postgres { pool: PgPool, database_name: String },
}

impl TestDatabase {
    /// Creates new test database using the best available backend.
    /// Creates database based on environment configuration.
    pub async fn new() -> Result<Self> {
        if std::env::var("KAPSEL_TEST_BACKEND").as_deref() == Ok("postgres")
            || std::env::var("CI").is_ok()
        {
            Self::new_postgres().await
        } else {
            Self::new_sqlite().await
        }
    }

    /// Creates SQLite in-memory database.
    pub async fn new_sqlite() -> Result<Self> {
        let database_url = "sqlite::memory:".to_string();
        let pool = sqlx::SqlitePool::connect(&database_url)
            .await
            .context("Failed to create SQLite test database")?;

        let db = Self::Sqlite { pool };
        db.run_migrations().await?;
        Ok(db)
    }

    /// Creates PostgreSQL database connection using existing postgres-test
    /// container.
    pub async fn new_postgres() -> Result<Self> {
        let database_name = "kapsel_test".to_string();

        // Read port from DATABASE_URL or default to 5432 (CI default)
        let port = std::env::var("DATABASE_URL")
            .ok()
            .and_then(|url| {
                url.split(':')
                    .nth(4)
                    .and_then(|port_str| port_str.split('/').next())
                    .and_then(|port_str| port_str.parse::<u16>().ok())
            })
            .unwrap_or(5432);

        let connect_options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database(&database_name);

        let pool = sqlx::PgPool::connect_with(connect_options)
            .await
            .context("Failed to connect to PostgreSQL test database")?;

        let db = Self::Postgres { pool, database_name };
        db.run_migrations().await?;
        Ok(db)
    }

    /// Returns connection pool for the underlying database.
    pub fn pool(&self) -> DatabasePool {
        match self {
            Self::Sqlite { pool } => DatabasePool::Sqlite(pool.clone()),
            Self::Postgres { pool, .. } => DatabasePool::Postgres(pool.clone()),
        }
    }

    /// Runs database migrations for the current backend.
    async fn run_migrations(&self) -> Result<()> {
        match self {
            Self::Sqlite { pool } => run_sqlite_migrations(pool).await,
            Self::Postgres { pool, .. } => run_postgres_migrations(pool).await,
        }
    }

    /// Seeds test data into database.
    pub async fn seed_test_data(&self) -> Result<()> {
        let pool = self.pool();
        seed_endpoints(&pool).await?;
        seed_webhook_events(&pool).await?;
        Ok(())
    }
}

/// Database pool abstraction.
#[derive(Clone)]
pub enum DatabasePool {
    Sqlite(Pool<Sqlite>),
    Postgres(PgPool),
}

impl DatabasePool {
    /// Begins database transaction.
    pub async fn begin(&self) -> Result<DatabaseTransaction> {
        match self {
            Self::Sqlite(pool) => {
                let tx = pool.begin().await?;
                Ok(DatabaseTransaction::Sqlite(tx))
            },
            Self::Postgres(pool) => {
                let tx = pool.begin().await?;
                Ok(DatabaseTransaction::Postgres(tx))
            },
        }
    }

    /// Execute a query and return the number of rows affected.
    pub async fn execute(&self, query: &str) -> Result<u64> {
        match self {
            Self::Sqlite(pool) => {
                let result = sqlx::query(query).execute(pool).await?;
                Ok(result.rows_affected())
            },
            Self::Postgres(pool) => {
                let result = sqlx::query(query).execute(pool).await?;
                Ok(result.rows_affected())
            },
        }
    }

    /// Get the underlying pool for direct SQLx operations.
    pub fn as_pool(&self) -> DatabasePoolRef<'_> {
        match self {
            Self::Sqlite(pool) => DatabasePoolRef::Sqlite(pool),
            Self::Postgres(pool) => DatabasePoolRef::Postgres(pool),
        }
    }
}

/// Reference to the underlying database pool for direct SQLx operations.
pub enum DatabasePoolRef<'a> {
    Sqlite(&'a Pool<Sqlite>),
    Postgres(&'a PgPool),
}

/// Transaction abstraction.
pub enum DatabaseTransaction {
    Sqlite(sqlx::Transaction<'static, Sqlite>),
    Postgres(sqlx::Transaction<'static, Postgres>),
}

impl DatabaseTransaction {
    /// Commits the transaction.
    pub async fn commit(self) -> Result<()> {
        match self {
            Self::Sqlite(tx) => tx.commit().await.map_err(Into::into),
            Self::Postgres(tx) => tx.commit().await.map_err(Into::into),
        }
    }

    /// Rolls back the transaction.
    pub async fn rollback(self) -> Result<()> {
        match self {
            Self::Sqlite(tx) => tx.rollback().await.map_err(Into::into),
            Self::Postgres(tx) => tx.rollback().await.map_err(Into::into),
        }
    }
}

/// Sets up test database and returns connection pool.
pub async fn setup_test_database() -> Result<DatabasePool> {
    let db = TestDatabase::new().await?;
    let pool = db.pool();

    #[allow(clippy::disallowed_methods)]
    Box::leak(Box::new(db));

    Ok(pool)
}

/// SQLite schema migrations.
async fn run_sqlite_migrations(pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS webhook_events (
            id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6)))),
            tenant_id TEXT NOT NULL,
            endpoint_id TEXT NOT NULL,
            source_event_id TEXT NOT NULL,
            idempotency_strategy TEXT NOT NULL,
            status TEXT NOT NULL,
            failure_count INTEGER NOT NULL DEFAULT 0,
            last_attempt_at TEXT,
            next_retry_at TEXT,
            headers TEXT NOT NULL,
            body BLOB NOT NULL,
            content_type TEXT NOT NULL,
            received_at TEXT NOT NULL DEFAULT (datetime('now')),
            delivered_at TEXT,
            failed_at TEXT,
            tigerbeetle_id TEXT,
            UNIQUE(tenant_id, endpoint_id, source_event_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS delivery_attempts (
            id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6)))),
            event_id TEXT NOT NULL REFERENCES webhook_events(id),
            attempt_number INTEGER NOT NULL,
            request_url TEXT NOT NULL,
            request_headers TEXT NOT NULL,
            response_status INTEGER,
            response_headers TEXT,
            response_body TEXT,
            attempted_at TEXT NOT NULL DEFAULT (datetime('now')),
            duration_ms INTEGER NOT NULL,
            error_type TEXT,
            error_message TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS endpoints (
            id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6)))),
            tenant_id TEXT NOT NULL,
            url TEXT NOT NULL,
            name TEXT NOT NULL,
            signing_secret TEXT,
            signature_header TEXT,
            max_retries INTEGER NOT NULL DEFAULT 10,
            timeout_seconds INTEGER NOT NULL DEFAULT 30,
            circuit_state TEXT NOT NULL DEFAULT 'closed',
            circuit_failure_count INTEGER NOT NULL DEFAULT 0,
            circuit_last_failure_at TEXT,
            circuit_half_open_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(tenant_id, name)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tenants (
            id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6)))),
            name TEXT NOT NULL,
            plan TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_webhook_events_status ON webhook_events(status, next_retry_at)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant ON webhook_events(tenant_id, received_at)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_delivery_attempts_event ON delivery_attempts(event_id, attempt_number)")
        .execute(pool)
        .await?;

    Ok(())
}

/// PostgreSQL schema migrations.
async fn run_postgres_migrations(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS webhook_events (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id UUID NOT NULL,
            endpoint_id UUID NOT NULL,
            source_event_id TEXT NOT NULL,
            idempotency_strategy TEXT NOT NULL,
            status TEXT NOT NULL,
            failure_count INTEGER NOT NULL DEFAULT 0,
            last_attempt_at TIMESTAMPTZ,
            next_retry_at TIMESTAMPTZ,
            headers JSONB NOT NULL,
            body BYTEA NOT NULL,
            content_type TEXT NOT NULL,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            delivered_at TIMESTAMPTZ,
            failed_at TIMESTAMPTZ,
            tigerbeetle_id UUID,
            UNIQUE(tenant_id, endpoint_id, source_event_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS delivery_attempts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            event_id UUID NOT NULL REFERENCES webhook_events(id),
            attempt_number INTEGER NOT NULL,
            request_url TEXT NOT NULL,
            request_headers JSONB NOT NULL,
            response_status INTEGER,
            response_headers JSONB,
            response_body TEXT,
            attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            duration_ms INTEGER NOT NULL,
            error_type TEXT,
            error_message TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS endpoints (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tenant_id UUID NOT NULL,
            url TEXT NOT NULL,
            name TEXT NOT NULL,
            signing_secret TEXT,
            signature_header TEXT,
            max_retries INTEGER NOT NULL DEFAULT 10,
            timeout_seconds INTEGER NOT NULL DEFAULT 30,
            circuit_state TEXT NOT NULL DEFAULT 'closed',
            circuit_failure_count INTEGER NOT NULL DEFAULT 0,
            circuit_last_failure_at TIMESTAMPTZ,
            circuit_half_open_at TIMESTAMPTZ,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE(tenant_id, name)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tenants (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL,
            plan TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_webhook_events_status
        ON webhook_events(status, next_retry_at)
        WHERE status IN ('pending', 'delivering')
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant ON webhook_events(tenant_id, received_at DESC)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_delivery_attempts_event ON delivery_attempts(event_id, attempt_number)")
        .execute(pool)
        .await?;

    Ok(())
}

/// Seeds test endpoints.
async fn seed_endpoints(pool: &DatabasePool) -> Result<()> {
    let tenant_id = Uuid::new_v4().to_string();

    match pool {
        DatabasePool::Sqlite(pool) => {
            sqlx::query(
                r#"
                INSERT INTO endpoints (tenant_id, name, url, signing_secret, max_retries)
                VALUES
                    (?, 'test-endpoint-1', 'https://example.com/webhook', 'secret123', 3),
                    (?, 'test-endpoint-2', 'https://example.org/hook', 'secret456', 5)
                "#,
            )
            .bind(&tenant_id)
            .bind(&tenant_id)
            .execute(pool)
            .await?;
        },
        DatabasePool::Postgres(pool) => {
            let tenant_uuid = Uuid::parse_str(&tenant_id)?;
            sqlx::query(
                r#"
                INSERT INTO endpoints (tenant_id, name, url, signing_secret, max_retries)
                VALUES
                    ($1, 'test-endpoint-1', 'https://example.com/webhook', 'secret123', 3),
                    ($1, 'test-endpoint-2', 'https://example.org/hook', 'secret456', 5)
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(tenant_uuid)
            .execute(pool)
            .await?;
        },
    }

    Ok(())
}

/// Seeds test webhook events.
async fn seed_webhook_events(pool: &DatabasePool) -> Result<()> {
    match pool {
        DatabasePool::Sqlite(pool) => {
            let endpoint: (String, String) =
                sqlx::query_as("SELECT id, tenant_id FROM endpoints LIMIT 1")
                    .fetch_one(pool)
                    .await?;

            sqlx::query(
                r#"
                INSERT OR IGNORE INTO webhook_events (
                    tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                    status, headers, body, content_type
                )
                VALUES
                    (?, ?, 'test-event-1', 'header', 'pending', '{}', 'test body 1', 'text/plain'),
                    (?, ?, 'test-event-2', 'header', 'delivered', '{}', 'test body 2', 'text/plain')
                "#,
            )
            .bind(&endpoint.1)
            .bind(&endpoint.0)
            .bind(&endpoint.1)
            .bind(&endpoint.0)
            .execute(pool)
            .await?;
        },
        DatabasePool::Postgres(pool) => {
            let endpoint: (Uuid, Uuid) =
                sqlx::query_as("SELECT id, tenant_id FROM endpoints LIMIT 1")
                    .fetch_one(pool)
                    .await?;

            sqlx::query(
                r#"
                INSERT INTO webhook_events (
                    tenant_id, endpoint_id, source_event_id, idempotency_strategy,
                    status, headers, body, content_type
                )
                VALUES
                    ($1, $2, 'test-event-1', 'header', 'pending', '{}', 'test body 1', 'text/plain'),
                    ($1, $2, 'test-event-2', 'header', 'delivered', '{}', 'test body 2', 'text/plain')
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(endpoint.1)
            .bind(endpoint.0)
            .execute(pool)
            .await?;
        },
    }

    Ok(())
}

/// Database test assertions.
pub mod assertions {
    use uuid::Uuid;

    use super::DatabasePool;

    /// Asserts event exists with expected status.
    pub async fn assert_event_status(
        pool: &DatabasePool,
        event_id: &str,
        expected_status: &str,
    ) -> Result<(), String> {
        let status: Option<String> = match pool {
            DatabasePool::Sqlite(pool) => {
                sqlx::query_scalar("SELECT status FROM webhook_events WHERE id = ?")
                    .bind(event_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| format!("Database query failed: {}", e))?
            },
            DatabasePool::Postgres(pool) => {
                let event_uuid =
                    Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;
                sqlx::query_scalar("SELECT status FROM webhook_events WHERE id = $1")
                    .bind(event_uuid)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| format!("Database query failed: {}", e))?
            },
        };

        match status {
            Some(actual) if actual == expected_status => Ok(()),
            Some(actual) => Err(format!(
                "Event {} has status '{}', expected '{}'",
                event_id, actual, expected_status
            )),
            None => Err(format!("Event {} not found", event_id)),
        }
    }

    /// Asserts delivery attempt count for event.
    pub async fn assert_delivery_attempts(
        pool: &DatabasePool,
        event_id: &str,
        expected_count: i64,
    ) -> Result<(), String> {
        let count: i64 = match pool {
            DatabasePool::Sqlite(pool) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = ?")
                    .bind(event_id)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| format!("Database query failed: {}", e))?
            },
            DatabasePool::Postgres(pool) => {
                let event_uuid =
                    Uuid::parse_str(event_id).map_err(|e| format!("Invalid UUID: {}", e))?;
                sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
                    .bind(event_uuid)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| format!("Database query failed: {}", e))?
            },
        };

        if count != expected_count {
            return Err(format!(
                "Event {} has {} delivery attempts, expected {}",
                event_id, count, expected_count
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_setup_succeeds() {
        let pool = setup_test_database().await.unwrap();

        let result = match pool {
            DatabasePool::Sqlite(pool) => {
                sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool).await.unwrap()
            },
            DatabasePool::Postgres(pool) => {
                sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&pool).await.unwrap()
            },
        };

        assert_eq!(result, 1);
    }

    #[tokio::test]
    async fn migrations_create_tables() {
        let pool = setup_test_database().await.unwrap();

        let tables: Vec<String> = match pool {
            DatabasePool::Sqlite(pool) => sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .unwrap(),
            DatabasePool::Postgres(pool) => sqlx::query_scalar(
                "SELECT table_name FROM information_schema.tables
                     WHERE table_schema = 'public' ORDER BY table_name",
            )
            .fetch_all(&pool)
            .await
            .unwrap(),
        };

        assert!(tables.contains(&"webhook_events".to_string()));
        assert!(tables.contains(&"delivery_attempts".to_string()));
        assert!(tables.contains(&"endpoints".to_string()));
        assert!(tables.contains(&"tenants".to_string()));
    }
}
