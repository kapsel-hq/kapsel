//! Database access layer implementing the repository pattern for webhook
//! persistence.
//!
//! The repository layer acts as an anti-corruption layer, translating between
//! domain models and database schemas. This isolation allows schema evolution
//! without breaking domain logic.
//!
//! All database operations MUST go through these repositories. Direct SQL
//! queries outside this module are forbidden to maintain consistency.

use std::sync::Arc;

use sqlx::PgPool;

pub mod delivery_attempts;
pub mod endpoints;
pub mod tenants;
pub mod webhook_events;

use crate::error::Result;

/// Container for all repository instances providing unified database access.
///
/// The `Storage` struct is the entry point for all database operations in
/// Kapsel. It manages a shared connection pool and provides type-safe access to
/// each domain repository.
#[derive(Clone)]
pub struct Storage {
    /// Repository for webhook event operations.
    pub webhook_events: Arc<webhook_events::Repository>,

    /// Repository for delivery attempt tracking.
    pub delivery_attempts: Arc<delivery_attempts::Repository>,

    /// Repository for endpoint configuration.
    pub endpoints: Arc<endpoints::Repository>,

    /// Repository for tenant management.
    pub tenants: Arc<tenants::Repository>,
}

impl Storage {
    /// Creates a new storage instance with the given connection pool.
    ///
    /// All repositories share the same pool with Arc for efficient resource
    /// usage.
    pub fn new(pool: PgPool) -> Self {
        let pool = Arc::new(pool);

        Self {
            webhook_events: Arc::new(webhook_events::Repository::new(pool.clone())),
            delivery_attempts: Arc::new(delivery_attempts::Repository::new(pool.clone())),
            endpoints: Arc::new(endpoints::Repository::new(pool.clone())),
            tenants: Arc::new(tenants::Repository::new(pool)),
        }
    }

    /// Performs a health check on the database connection.
    ///
    /// Executes a simple query to verify database connectivity. Used by
    /// the `/health/ready` endpoint for Kubernetes readiness probes.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Database` if the connection is unhealthy or
    /// the query times out.
    pub async fn health_check(&self) -> Result<()> {
        let _: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&*self.webhook_events.pool()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn storage_can_be_created() {
        // This test verifies the Storage struct can be instantiated
        // Actual database testing happens in integration tests
        let pool = sqlx::PgPool::connect_lazy("postgresql://test").unwrap();
        let _storage = Storage::new(pool);
    }
}
