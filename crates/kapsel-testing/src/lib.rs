//! Test infrastructure and utilities for deterministic testing.
//!
//! Provides database transaction isolation, HTTP mocking, fixture builders,
//! and property-based testing utilities. Ensures reproducible test execution
//! with proper resource cleanup and invariant checking.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

// Standard library imports
use std::{collections::HashMap, sync::Arc};

// External crate imports
use anyhow::Result;
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

impl WebhookEventData {
    /// Number of delivery attempts made (failure_count + 1 for initial
    /// attempt).
    pub fn attempt_count(&self) -> u32 {
        u32::try_from(self.failure_count + 1).unwrap_or(1)
    }
}
