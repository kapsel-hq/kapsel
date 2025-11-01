//! Test infrastructure and utilities for deterministic testing.
//!
//! Provides database transaction isolation, HTTP mocking, fixture builders,
//! and property-based testing utilities. Ensures reproducible test execution
//! with proper resource cleanup and invariant checking.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use std::sync::Arc;

use kapsel_core::storage::Storage;
use kapsel_delivery::DeliveryEngine;
use sqlx::PgPool;
use tokio::sync::RwLock;

// Public modules
pub mod database;
pub mod events;
pub mod fixtures;
pub mod http;
pub mod invariants;
pub mod property;
pub mod scenario;
pub mod time;

// Implementation modules
mod attestation;
mod database_helpers;
mod delivery;
mod env_core;
mod snapshots;

// Public re-exports
pub use database::TestDatabase;
pub use database_helpers::WebhookEventData;
pub use env_core::TestEnvBuilder;
pub use fixtures::{EndpointBuilder, TestEndpoint, TestWebhook, WebhookBuilder};
pub use http::{MockEndpoint, MockResponse, MockServer};
pub use invariants::{assertions as invariant_assertions, strategies, Invariants};
pub use kapsel_core::{
    models::{EndpointId, EventId, TenantId},
    storage::{merkle_leaves::AttestationLeafInfo, signed_tree_heads::SignedTreeHeadInfo},
    Clock,
};
pub use scenario::{FailureKind, ScenarioBuilder};
pub use time::TestClock;

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

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::TestEnv;

    #[tokio::test]
    async fn run_isolated_test_api_works() -> Result<()> {
        TestEnv::run_isolated_test(|env| async move {
            env.verify_connection().await?;
            let tenant_id = env.create_tenant("test-tenant").await?;
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
    #[allow(clippy::panic)]
    async fn run_isolated_test_handles_test_panics() -> Result<()> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                TestEnv::run_isolated_test(|_env| async move {
                    panic!("Test panic for cleanup verification");
                })
                .await
            })
        }));

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn run_isolated_test_handles_test_errors() -> Result<()> {
        let result = TestEnv::run_isolated_test(|_env| async move {
            anyhow::bail!("Test error for cleanup verification");
        })
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Test error for cleanup verification"));
        Ok(())
    }
}
