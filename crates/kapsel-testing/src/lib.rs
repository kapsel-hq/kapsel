//! Test infrastructure and utilities for deterministic testing.
//!
//! Provides database transaction isolation, HTTP mocking, fixture builders,
//! and property-based testing utilities. Ensures reproducible test execution
//! with proper resource cleanup and invariant checking.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

// Standard library imports
use std::{collections::HashMap, future::Future, sync::Arc};

// External crate imports
use anyhow::{Context, Result};
use kapsel_core::EventStatus;
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

// Public modules
pub mod database;
pub mod events;
pub mod fixtures;
pub mod http;
pub mod invariants;
pub mod property;
pub mod scenario;
pub mod time;

pub use database::TestDatabase;
pub use env_core::TestEnvBuilder;
pub use fixtures::{EndpointBuilder, TestEndpoint, TestWebhook, WebhookBuilder};
pub use http::{MockEndpoint, MockResponse, MockServer};
pub use invariants::{assertions as invariant_assertions, strategies, Invariants};
pub use kapsel_core::{
    models::{EndpointId, EventId, TenantId},
    storage::{merkle_leaves::AttestationLeafInfo, signed_tree_heads::SignedTreeHeadInfo, Storage},
    Clock,
};
use kapsel_delivery::DeliveryEngine;
pub use scenario::{FailureKind, ScenarioBuilder};
pub use time::TestClock;

// These implementation modules extend TestEnv with various methods
mod attestation;
mod database_helpers;
mod delivery;
mod env_core;
mod snapshots;

type InvariantCheckFn = Box<
    dyn for<'a> Fn(
        &'a mut TestEnv,
    )
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>,
>;

type AssertionFn = Box<dyn Fn(&mut TestEnv) -> Result<()>>;

/// Test environment with database isolation for integration testing.
///
/// Provides a complete testing environment with:
/// - Database isolation (per-test or transaction-based)
/// - HTTP mocking for external services
/// - Deterministic time control
/// - Attestation service integration
/// - Helper methods for test data setup
pub struct TestEnv {
    /// HTTP mock server for external API simulation
    pub http_mock: http::MockServer,
    /// Deterministic clock for time-based testing
    pub clock: time::TestClock,
    /// Database handle for this test environment
    database: PgPool,
    /// Storage layer providing repository access
    storage: Arc<Storage>,
    /// Optional attestation service for testing delivery capture
    attestation_service: Option<Arc<RwLock<kapsel_attestation::MerkleService>>>,
    /// Unique identifier for this test run to ensure data isolation
    test_run_id: String,
    /// Flag to distinguish isolated vs shared database tests
    is_isolated: bool,
    /// Production delivery engine for integration testing
    delivery_engine: Option<DeliveryEngine>,
}

/// Simplified webhook event data for invariant checking.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WebhookEventData {
    /// Event identifier
    pub id: EventId,
    /// Tenant identifier
    pub tenant_id: TenantId,
    /// Endpoint identifier
    pub endpoint_id: Uuid,
    /// Source event identifier
    pub source_event_id: String,
    /// Idempotency strategy used
    pub idempotency_strategy: String,
    /// Current event status
    pub status: EventStatus,
    /// Number of failed delivery attempts
    pub failure_count: i32,
    /// Last delivery attempt time
    pub last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Next scheduled retry time
    pub next_retry_at: Option<chrono::DateTime<chrono::Utc>>,
    /// HTTP headers as JSON
    pub headers: serde_json::Value,
    /// Request body as bytes
    pub body: Vec<u8>,
    /// Content type header
    pub content_type: String,
    /// Payload size in bytes
    pub payload_size: i32,
    /// Whether signature is valid
    pub signature_valid: Option<bool>,
    /// Signature validation error
    pub signature_error: Option<String>,
    /// When event was received
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// When event was delivered (if successful)
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When event permanently failed (if applicable)
    pub failed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl TestEnv {
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
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use kapsel_testing::TestEnv;
    /// # use anyhow::Result;
    /// #[tokio::test]
    /// async fn test_delivery_engine() -> Result<()> {
    ///     TestEnv::run_isolated_test(|mut env| async move {
    ///         let tenant_id = env.create_tenant("test-tenant").await?;
    ///         // ... test logic ...
    ///
    ///         env.run_delivery_cycle().await?;
    ///         Ok(())
    ///     })
    ///     .await
    /// }
    /// ```
    pub async fn run_isolated_test<F, Fut>(test_fn: F) -> Result<()>
    where
        F: FnOnce(TestEnv) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        use database::{
            create_admin_pool, create_database_from_template, drop_database_immediate,
            ensure_template_database_exists,
        };
        use futures::FutureExt;

        // 1. ENSURE TEMPLATE EXISTS (runs once per suite, fast and lock-free)
        ensure_template_database_exists()
            .await
            .context("failed to ensure template database exists")?;

        // 2. SETUP: Create the unique, isolated database for this single test
        let admin_pool = create_admin_pool().await.context("failed to create admin pool")?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let db_name = format!("test_{}_{}", timestamp, Uuid::new_v4().simple());

        create_database_from_template(&admin_pool, &db_name)
            .await
            .with_context(|| format!("failed to create isolated database: {}", db_name))?;

        // Create a TestEnv specifically for this isolated DB
        let env = Self::build_for_isolated_db(&db_name)
            .await
            .with_context(|| format!("failed to build TestEnv for database: {}", db_name))?;

        // 3. EXECUTION: Run the actual test logic
        // Use catch_unwind to ensure teardown runs even if the test panics
        let test_result = std::panic::AssertUnwindSafe(test_fn(env)).catch_unwind().await;

        // 4. GUARANTEED TEARDOWN:
        // This code runs *after* the test function has completed
        // We `await` the cleanup, so it is guaranteed to finish
        if let Err(e) = drop_database_immediate(&admin_pool, &db_name).await {
            // Log the cleanup failure but don't mask the original test failure
            tracing::error!(database = %db_name, error = %e, "CRITICAL: Failed to clean up isolated test database!");
        }

        // 5. Return the original test result
        match test_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),                   // Test returned an error
            Err(e) => std::panic::resume_unwind(e), // Test panicked
        }
    }

    /// Build TestEnv for a specific isolated database.
    async fn build_for_isolated_db(database_name: &str) -> Result<Self> {
        use database::create_database_pool;

        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        let database = create_database_pool(database_name)
            .await
            .with_context(|| format!("failed to create pool for database: {}", database_name))?;

        let test_run_id = Uuid::new_v4().simple().to_string();
        let http_mock = http::MockServer::start().await;
        let clock = time::TestClock::new();
        let clock_arc: Arc<dyn kapsel_core::Clock> = Arc::new(clock.clone());
        let storage = Arc::new(kapsel_core::storage::Storage::new(database.clone(), &clock_arc));

        // Create delivery engine with default configuration
        let delivery_config = kapsel_delivery::DeliveryConfig {
            worker_count: 1, // Single worker for determinism
            batch_size: 10,
            poll_interval: std::time::Duration::from_millis(100),
            shutdown_timeout: std::time::Duration::from_secs(5),
            client_config: kapsel_delivery::ClientConfig::default(),
            default_retry_policy: kapsel_delivery::retry::RetryPolicy {
                max_attempts: 10,
                base_delay: std::time::Duration::from_secs(1),
                max_delay: std::time::Duration::from_secs(600),
                jitter_factor: 0.0, // Zero jitter for deterministic tests
                backoff_strategy: kapsel_delivery::retry::BackoffStrategy::Exponential,
            },
        };

        let event_handler =
            Arc::new(kapsel_core::NoOpEventHandler) as Arc<dyn kapsel_core::EventHandler>;

        let delivery_engine = Some(
            kapsel_delivery::DeliveryEngine::with_event_handler(
                database.clone(),
                delivery_config,
                clock_arc,
                event_handler,
            )
            .context("failed to create delivery engine")?,
        );

        Ok(TestEnv {
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

impl WebhookEventData {
    /// Number of delivery attempts made (failure_count + 1 for initial
    /// attempt).
    pub fn attempt_count(&self) -> u32 {
        u32::try_from(self.failure_count + 1).unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_isolated_test_api_works() -> Result<()> {
        TestEnv::run_isolated_test(|env| async move {
            // Verify database connection works
            env.verify_connection().await?;

            // Test basic database operations - no transaction needed for isolated tests
            let tenant_id = env.create_tenant("test-tenant").await?;

            // Verify tenant was created
            let tenant = env
                .storage()
                .tenants
                .find_by_id(tenant_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("tenant not found"))?;
            assert!(tenant.name.starts_with("test-tenant"));

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn run_isolated_test_handles_test_panics() -> Result<()> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                TestEnv::run_isolated_test(|_env| async move {
                    panic!("Test panic for cleanup verification");
                })
                .await
            })
        }));

        // The panic should be caught and re-thrown, but database cleanup should still
        // happen
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn run_isolated_test_handles_test_errors() -> Result<()> {
        let result = TestEnv::run_isolated_test(|_env| async move {
            anyhow::bail!("Test error for cleanup verification");
        })
        .await;

        // The error should be propagated, but database cleanup should still happen
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Test error for cleanup verification"));
        Ok(())
    }
}
