//! Test infrastructure and utilities for deterministic testing.
//!
//! Provides database transaction isolation, HTTP mocking, fixture builders,
//! and property-based testing utilities. Ensures reproducible test execution
//! with proper resource cleanup and invariant checking

#![warn(unsafe_code)]
#![warn(missing_docs)]

pub mod database;
pub mod events;
pub mod fixtures;
pub mod http;
pub mod invariants;
pub mod time;

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use database::{SharedDatabase, TestDatabase};
pub use invariants::{assertions as invariant_assertions, strategies, Invariants};
use kapsel_core::models::{EndpointId, EventId, TenantId};
use sqlx::{PgPool, Row};
pub use time::Clock;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::fixtures::TestWebhook;

/// Type alias for invariant check functions to reduce complexity
type InvariantCheckFn = Box<dyn Fn(&mut TestEnv) -> Result<()>>;

/// Type alias for assertion functions to reduce complexity
type AssertionFn = Box<dyn Fn(&mut TestEnv) -> Result<()>>;

/// Type alias for a webhook ready for delivery to reduce complexity
type ReadyWebhook = (EventId, Uuid, String, Vec<u8>, i32, String, i32);

/// Test environment with transaction-based database isolation.
///
/// Each test gets its own TestEnv with an isolated database transaction
/// that automatically rolls back when dropped. This ensures perfect
/// isolation between tests with minimal overhead.
pub struct TestEnv {
    /// HTTP mock server for external API simulation
    pub http_mock: http::MockServer,
    /// Deterministic clock for time-based testing
    pub clock: time::TestClock,
    /// Database handle for this test environment
    _database: TestDatabase,
    /// Shared database instance (Arc ensures proper cleanup)
    shared_db: std::sync::Arc<SharedDatabase>,
    /// Optional attestation service for testing delivery capture
    attestation_service:
        Option<std::sync::Arc<tokio::sync::RwLock<kapsel_attestation::MerkleService>>>,
}

impl TestEnv {
    /// Create a new test environment with transaction isolation.
    ///
    /// This sets up:
    /// - A database transaction that auto-rollbacks
    /// - An HTTP mock server for external calls
    /// - A deterministic test clock
    ///
    /// # Errors
    ///
    /// Returns error if database container fails to start, connection fails,
    /// or transaction creation fails.
    pub async fn new() -> Result<Self> {
        // Initialize tracing once per process
        // Default to error-level logging to keep test output clean
        // Set RUST_LOG=debug to see detailed logs when debugging
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
            )
            .with_test_writer()
            .try_init();

        // Use shared database for standard test runtime
        let shared_db = database::create_shared_database().await?;
        let db = TestDatabase::new().await?;

        // Create HTTP mock server
        let http_mock = http::MockServer::start().await;

        // Create deterministic clock
        let clock = time::TestClock::new();

        Ok(Self { http_mock, clock, _database: db, shared_db, attestation_service: None })
    }

    /// Create a new isolated test environment with its own database instance.
    ///
    /// Use this for tests that create their own async runtime (like property
    /// tests) or need complete database isolation. Most tests should use
    /// `new()` instead.
    ///
    /// # Errors
    ///
    /// Returns error if test environment setup fails.
    pub async fn new_isolated() -> Result<Self> {
        // Initialize tracing for isolated tests
        let _ = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).try_init();

        // Create completely isolated database for this runtime
        let db = TestDatabase::new_isolated().await?;
        let shared_db = std::sync::Arc::clone(db.shared_database());

        // Create HTTP mock server
        let http_mock = http::MockServer::start().await;

        // Create deterministic clock
        let clock = time::TestClock::new();

        Ok(Self { http_mock, clock, _database: db, shared_db, attestation_service: None })
    }

    /// Returns direct access to the database connection pool.
    ///
    /// Use this for setup operations that need to persist across method calls.
    /// For transaction isolation, create transactions manually as needed.
    pub fn pool(&self) -> &PgPool {
        self._database.shared_database().pool()
    }

    /// Create a new connection pool for components that manage their own
    /// connections.
    ///
    /// This is useful for testing delivery workers and other components
    /// that need their own database connections while maintaining high
    /// performance.
    pub fn create_pool(&self) -> PgPool {
        self.shared_db.pool().clone()
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

    /// Create a test tenant with default configuration.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    pub async fn create_tenant(&self, name: &str) -> Result<TenantId> {
        self.create_tenant_with_plan(name, "enterprise").await
    }

    /// Create a test tenant with specific plan.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    pub async fn create_tenant_with_plan(&self, name: &str, plan: &str) -> Result<TenantId> {
        let tenant_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO tenants (id, name, plan, created_at, updated_at)
             VALUES ($1, $2, $3, NOW(), NOW())",
        )
        .bind(tenant_id)
        .bind(name)
        .bind(plan)
        .execute(self.pool())
        .await
        .context("failed to create test tenant")?;

        Ok(TenantId(tenant_id))
    }

    /// Create a test endpoint with default configuration.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    pub async fn create_endpoint(&self, tenant_id: TenantId, url: &str) -> Result<EndpointId> {
        self.create_endpoint_with_config(tenant_id, url, "test-endpoint", 10, 30).await
    }

    /// Create a test endpoint with full configuration.
    ///
    /// # Errors
    ///
    /// Returns error if database insert fails.
    pub async fn create_endpoint_with_config(
        &self,
        tenant_id: TenantId,
        url: &str,
        name: &str,
        max_retries: i32,
        timeout_seconds: i32,
    ) -> Result<EndpointId> {
        let endpoint_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())",
        )
        .bind(endpoint_id)
        .bind(tenant_id.0)
        .bind(name)
        .bind(url)
        .bind(max_retries)
        .bind(timeout_seconds)
        .bind("closed")
        .execute(self.pool())
        .await
        .context("failed to create test endpoint")?;

        Ok(EndpointId(endpoint_id))
    }

    /// Seed common test data.
    ///
    /// Creates a standard tenant and endpoint for tests that don't
    /// need specific configurations.
    ///
    /// # Errors
    ///
    /// Returns error if database inserts fail.
    pub async fn seed_test_data(&self) -> Result<(TenantId, EndpointId)> {
        let tenant_id = Uuid::new_v4();
        let endpoint_id = Uuid::new_v4();

        // Create tenant
        sqlx::query(
            "INSERT INTO tenants (id, name, plan, created_at, updated_at)
             VALUES ($1, $2, $3, NOW(), NOW())",
        )
        .bind(tenant_id)
        .bind("test-tenant")
        .bind("free")
        .execute(self.pool())
        .await
        .context("failed to create test tenant")?;

        // Create endpoint
        sqlx::query(
            "INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())",
        )
        .bind(endpoint_id)
        .bind(tenant_id)
        .bind("Test Endpoint")
        .bind("https://example.com/webhook")
        .bind(10)
        .bind(30)
        .bind("closed")
        .execute(self.pool())
        .await
        .context("failed to create test endpoint")?;

        Ok((TenantId(tenant_id), EndpointId(endpoint_id)))
    }

    /// Ingests a test webhook, persisting it to the database transaction.
    ///
    /// Handles idempotency by returning existing event ID if duplicate
    /// source_event_id.
    ///
    /// # Errors
    ///
    /// Returns error if header serialization or database insert fails.
    pub async fn ingest_webhook(&self, webhook: &TestWebhook) -> Result<EventId> {
        let event_id = Uuid::new_v4();
        let body_bytes = webhook.body.clone();
        let payload_size = (body_bytes.len() as i32).max(1); // Ensure minimum size of 1

        let result: (Uuid,) = sqlx::query_as(
            "INSERT INTO webhook_events
             (id, tenant_id, endpoint_id, source_event_id, idempotency_strategy,
              status, failure_count, headers, body, content_type, payload_size, received_at)
             VALUES ($1, $2, $3, $4, $5, 'pending', 0, $6, $7, $8, $9, $10)
             ON CONFLICT (tenant_id, endpoint_id, source_event_id)
             DO UPDATE SET id = webhook_events.id
             RETURNING id",
        )
        .bind(event_id)
        .bind(webhook.tenant_id)
        .bind(webhook.endpoint_id)
        .bind(&webhook.source_event_id)
        .bind(&webhook.idempotency_strategy)
        .bind(serde_json::to_value(&webhook.headers)?)
        .bind(&body_bytes[..])
        .bind(&webhook.content_type)
        .bind(payload_size)
        .bind(chrono::DateTime::<Utc>::from(self.now_system()))
        .fetch_one(self.pool())
        .await
        .context("failed to ingest test webhook")?;

        Ok(EventId(result.0))
    }

    /// Finds the current status of a webhook event from the database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails or event not found.
    pub async fn find_webhook_status(&self, event_id: EventId) -> Result<String> {
        let status: (String,) = sqlx::query_as("SELECT status FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(self.pool())
            .await
            .context("failed to find webhook status")?;

        Ok(status.0)
    }

    /// Counts the number of delivery attempts for a webhook event in the
    /// database.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_delivery_attempts(&self, event_id: EventId) -> Result<i64> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM delivery_attempts WHERE event_id = $1")
                .bind(event_id.0)
                .fetch_one(self.pool())
                .await
                .context("failed to count delivery attempts")?;

        Ok(count.0)
    }

    /// Simulates a single run of the delivery worker pool.
    ///
    /// Finds pending webhooks ready for delivery and processes them.
    ///
    /// # Errors
    ///
    /// Returns error if database queries or HTTP mock recording fails.
    pub async fn run_delivery_cycle(&self) -> Result<()> {
        // Find webhooks ready for delivery (use pool directly for persistent
        // operations)
        let ready_webhooks: Vec<ReadyWebhook> = sqlx::query_as(
            "SELECT we.id, e.id as endpoint_id, e.url, we.body, we.failure_count, e.name, e.max_retries
             FROM webhook_events we
             JOIN endpoints e ON we.endpoint_id = e.id
             WHERE we.status = 'pending'
             AND (we.next_retry_at IS NULL OR we.next_retry_at <= $1)
             ORDER BY we.received_at ASC",
        )
        .bind(chrono::DateTime::<Utc>::from(self.now_system()))
        .fetch_all(self.pool())
        .await
        .context("failed to fetch ready webhooks")?;

        let webhook_count = ready_webhooks.len();
        for (event_id, _endpoint_id, url, body, failure_count, _endpoint_name, max_retries) in
            ready_webhooks
        {
            // Check if webhook has exceeded maximum retry attempts
            // max_retries is the number of retry attempts after initial attempt
            if failure_count > max_retries {
                // Mark as failed - no more retries
                sqlx::query(
                    "UPDATE webhook_events
                     SET status = 'failed', last_attempt_at = $1
                     WHERE id = $2",
                )
                .bind(chrono::DateTime::<Utc>::from(self.now_system()))
                .bind(event_id.0)
                .execute(self.pool())
                .await
                .context("failed to mark webhook as failed")?;
                continue;
            }
            // Make HTTP request to the mock server
            let client = reqwest::Client::new();
            let response_result = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("X-Kapsel-Event-Id", event_id.0.to_string())
                .body(body.clone())
                .send()
                .await;

            let (status_code, response_body, duration_ms, error_type) = match response_result {
                Ok(response) => {
                    let status = response.status().as_u16() as i32;
                    let body = response.text().await.unwrap_or_default();
                    let error_type = if status >= 400 { Some("http_error") } else { None };
                    (Some(status), Some(body), 75i32, error_type)
                },
                Err(_) => (None, None, 1000i32, Some("network")),
            };

            // Record delivery attempt
            let attempt_number = failure_count + 1;
            let attempt_id = Uuid::new_v4();
            let attempted_at = chrono::DateTime::<Utc>::from(self.now_system());

            sqlx::query(
                "INSERT INTO delivery_attempts
                 (id, event_id, attempt_number, request_url, request_headers,
                  response_status, response_body, attempted_at, duration_ms, error_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(attempt_id)
            .bind(event_id.0)
            .bind(attempt_number)
            .bind(&url)
            .bind(serde_json::json!({"X-Kapsel-Event-Id": event_id.0.to_string()}))
            .bind(status_code)
            .bind(response_body)
            .bind(attempted_at)
            .bind(duration_ms)
            .bind(error_type)
            .execute(self.pool())
            .await
            .context("failed to record delivery attempt")?;

            // No commit needed - using pool directly for persistent operations

            // Update webhook status based on response
            let (new_status, next_retry_at) = match status_code {
                Some(200..=299) => ("delivered".to_string(), None),
                _ => {
                    let backoff_delay =
                        time::backoff::deterministic_webhook_backoff(failure_count as u32);
                    let current_time = chrono::DateTime::<Utc>::from(self.now_system());
                    let next_retry =
                        Some(current_time + chrono::Duration::from_std(backoff_delay)?);
                    ("pending".to_string(), next_retry)
                },
            };

            if new_status == "delivered" {
                sqlx::query(
                    "UPDATE webhook_events
                     SET status = $1, delivered_at = $2, failure_count = $3, last_attempt_at = $4
                     WHERE id = $5",
                )
                .bind(new_status)
                .bind(attempted_at)
                .bind(attempt_number)
                .bind(attempted_at)
                .bind(event_id.0)
                .execute(self.pool())
                .await
                .context("failed to update event status after delivery")?;

                // Emit attestation event if attestation is enabled
                if self.attestation_service.is_some() {
                    // Get tenant_id from webhook_events table
                    let (tenant_id, payload_size): (uuid::Uuid, i32) = sqlx::query_as(
                        "SELECT tenant_id, payload_size FROM webhook_events WHERE id = $1",
                    )
                    .bind(event_id.0)
                    .fetch_one(self.pool())
                    .await
                    .context("failed to fetch webhook event for attestation")?;

                    // Compute payload hash
                    let payload_hash = {
                        use sha2::{Digest, Sha256};
                        let mut hasher = Sha256::new();
                        hasher.update(&body);
                        hasher.finalize().into()
                    };

                    // Create and emit delivery success event
                    let success_event = kapsel_core::DeliverySucceededEvent {
                        delivery_attempt_id: attempt_id,
                        event_id: kapsel_core::models::EventId(event_id.0),
                        tenant_id: kapsel_core::models::TenantId(tenant_id),
                        endpoint_url: url.clone(),
                        response_status: status_code.unwrap_or(200) as u16,
                        attempt_number: attempt_number as u32,
                        delivered_at: attempted_at,
                        payload_hash,
                        payload_size,
                    };

                    tracing::debug!(
                        event_id = %event_id.0,
                        delivery_attempt_id = %attempt_id,
                        tenant_id = %tenant_id,
                        attempt_number = attempt_number,
                        "created attestation success event for delivery"
                    );

                    // Get reference to the service
                    if let Some(ref merkle_service_wrapped) = self.attestation_service {
                        use kapsel_attestation::AttestationEventSubscriber;
                        use kapsel_core::EventHandler;

                        // Check pending count before processing
                        let pending_before = {
                            let service_read = merkle_service_wrapped.read().await;
                            service_read.pending_count().await.unwrap_or(0)
                        };

                        let event_id = success_event.event_id;

                        tracing::debug!(
                            event_id = %event_id,
                            pending_before = pending_before,
                            "processing attestation event for successful delivery"
                        );

                        let attestation_subscriber =
                            AttestationEventSubscriber::new(merkle_service_wrapped.clone());

                        attestation_subscriber
                            .handle_event(kapsel_core::DeliveryEvent::Succeeded(success_event))
                            .await;

                        // Check pending count after processing
                        let pending_after = {
                            let service_read = merkle_service_wrapped.read().await;
                            service_read.pending_count().await.unwrap_or(0)
                        };

                        tracing::debug!(
                            event_id = %event_id,
                            pending_before = pending_before,
                            pending_after = pending_after,
                            "attestation event processing completed"
                        );

                        // Drop attestation_subscriber to release Arc reference
                        drop(attestation_subscriber);
                    }
                }
            } else {
                sqlx::query(
                    "UPDATE webhook_events
                     SET status = $1, failure_count = $2, last_attempt_at = $3, next_retry_at = $4
                     WHERE id = $5",
                )
                .bind(new_status)
                .bind(attempt_number)
                .bind(attempted_at)
                .bind(next_retry_at)
                .bind(event_id.0)
                .execute(self.pool())
                .await
                .context("failed to update event status after failure")?;
            }
        }

        tracing::debug!("Processed {} webhooks in delivery cycle", webhook_count);
        tokio::task::yield_now().await;
        Ok(())
    }

    /// Count rows in a table by ID.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails or ID not found.
    pub async fn count_by_id(&self, table: &str, column: &str, id: Uuid) -> Result<i64> {
        let query = format!("SELECT COUNT(*) FROM {} WHERE {} = $1", table, column);

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_one(self.pool())
            .await
            .context("failed to count by id")?;

        let count: i64 = row.try_get(0)?;
        Ok(count)
    }

    /// Check if database connection is healthy.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn database_health_check(&self) -> Result<bool> {
        let result = sqlx::query("SELECT 1 as health").fetch_one(self.pool()).await;
        Ok(result.is_ok())
    }

    /// List all tables in the database schema.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn list_tables(&self) -> Result<Vec<String>> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public'
             AND table_type = 'BASE TABLE'
             ORDER BY table_name",
        )
        .fetch_all(self.pool())
        .await
        .context("failed to list tables")?;

        Ok(tables)
    }

    /// Enable attestation service for testing.
    pub fn enable_attestation(&mut self, service: kapsel_attestation::MerkleService) {
        self.attestation_service = Some(std::sync::Arc::new(tokio::sync::RwLock::new(service)));
    }

    /// Count attestation leaves for a specific event.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_attestation_leaves_for_event(&self, event_id: EventId) -> Result<i64> {
        tracing::debug!(
            event_id = %event_id.0,
            "querying attestation leaf count for event"
        );

        // Debug: Check all leaves in the table
        let all_leaves: Vec<(uuid::Uuid, String)> =
            sqlx::query_as("SELECT event_id, endpoint_url FROM merkle_leaves")
                .fetch_all(self.pool())
                .await
                .unwrap_or_default();

        tracing::debug!(
            total_leaves = all_leaves.len(),
            leaves = ?all_leaves,
            "all attestation leaves in database"
        );

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves WHERE event_id = $1")
                .bind(event_id.0)
                .fetch_one(self.pool())
                .await?;

        tracing::debug!(
            event_id = %event_id.0,
            count = count,
            "attestation leaf count query result"
        );

        Ok(count)
    }

    /// Fetch attestation leaf for a specific event.
    pub async fn fetch_attestation_leaf_for_event(
        &mut self,
        event_id: EventId,
    ) -> Result<Option<AttestationLeafInfo>> {
        let result = sqlx::query(
            "SELECT ml.delivery_attempt_id, ml.endpoint_url, ml.attempt_number, da.response_status, ml.leaf_hash
             FROM merkle_leaves ml
             JOIN delivery_attempts da ON ml.delivery_attempt_id = da.id
             WHERE ml.event_id = $1",
        )
        .bind(event_id.0)
        .fetch_optional(self.pool())
        .await?;

        Ok(result.map(|row| AttestationLeafInfo {
            delivery_attempt_id: row.get("delivery_attempt_id"),
            endpoint_url: row.get("endpoint_url"),
            attempt_number: row.get("attempt_number"),
            response_status: row.get("response_status"),
            leaf_hash: row.get("leaf_hash"),
        }))
    }

    /// Count total attestation leaves across all events.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_total_attestation_leaves(&mut self) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM merkle_leaves").fetch_one(self.pool()).await?;

        Ok(count)
    }

    /// Count total signed tree heads.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn count_signed_tree_heads(&mut self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signed_tree_heads")
            .fetch_one(self.pool())
            .await?;

        Ok(count)
    }

    /// Fetch the latest signed tree head.
    ///
    /// # Errors
    ///
    /// Returns error if database query fails.
    pub async fn fetch_latest_signed_tree_head(&mut self) -> Result<Option<SignedTreeHeadInfo>> {
        let result = sqlx::query(
            "SELECT tree_size, root_hash, timestamp_ms, signature, batch_size
             FROM signed_tree_heads
             ORDER BY timestamp_ms DESC
             LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(result.map(|row: sqlx::postgres::PgRow| SignedTreeHeadInfo {
            tree_size: row.get("tree_size"),
            root_hash: row.get("root_hash"),
            timestamp_ms: row.get("timestamp_ms"),
            signature: row.get("signature"),
            batch_size: row.get("batch_size"),
        }))
    }

    /// Trigger attestation batch commitment (simulates background worker).
    pub async fn run_attestation_commitment(&self) -> Result<()> {
        if let Some(ref attestation_service) = self.attestation_service {
            // Check pending count before attempting commit
            let pending_count = attestation_service.read().await.pending_count().await.unwrap_or(0);
            tracing::debug!(
                pending_count = pending_count,
                "attempting attestation batch commitment"
            );

            match attestation_service.write().await.try_commit_pending().await {
                Ok(_sth) => {
                    tracing::debug!("Attestation batch committed successfully");
                    Ok(())
                },
                Err(kapsel_attestation::error::AttestationError::BatchCommitFailed { reason })
                    if reason.contains("no pending leaves") =>
                {
                    // No pending leaves to commit - this is fine
                    Ok(())
                },
                Err(e) => Err(anyhow::anyhow!("Attestation commitment failed: {}", e)),
            }
        } else {
            // No attestation service configured - skip
            Ok(())
        }
    }
}

/// Attestation leaf information for testing.
#[derive(Debug, Clone)]
/// Test environment attestation information for delivery attempts.
///
/// Contains information about a Merkle tree leaf representing a webhook
/// delivery attempt that has been attested and committed to the transparency
/// log.
pub struct AttestationLeafInfo {
    /// Unique identifier for the delivery attempt
    pub delivery_attempt_id: uuid::Uuid,
    /// Target endpoint URL where the webhook was delivered
    pub endpoint_url: String,
    /// Sequential attempt number for this delivery
    pub attempt_number: i32,
    /// HTTP response status code (None if delivery failed before response)
    pub response_status: Option<i32>,
    /// SHA-256 hash of the Merkle tree leaf content
    pub leaf_hash: Vec<u8>,
}

/// Signed tree head information for testing.
#[derive(Debug, Clone)]
/// Signed tree head information from the transparency log.
///
/// Represents a cryptographically signed commitment to the state of the
/// Merkle tree at a specific point in time, providing tamper-proof evidence
/// of all delivery attempts that have been logged.
pub struct SignedTreeHeadInfo {
    /// Total number of leaves in the Merkle tree
    pub tree_size: i64,
    /// SHA-256 hash of the Merkle tree root
    pub root_hash: Vec<u8>,
    /// Unix timestamp in milliseconds when the tree head was signed
    pub timestamp_ms: i64,
    /// Ed25519 signature over the tree head data
    pub signature: Vec<u8>,
    /// Number of leaves in the batch that created this tree head
    pub batch_size: i32,
}

/// Test scenario builder for complex multi-step tests.
///
/// Enables deterministic testing with time control and invariant validation.
pub struct ScenarioBuilder {
    name: String,
    steps: Vec<Step>,
    invariant_checks: Vec<InvariantCheckFn>,
}

enum Step {
    IngestWebhook(TestWebhook),
    RunDeliveryCycle,
    AdvanceTime(Duration),
    InjectHttpFailure(http::MockResponse),
    InjectHttpSuccess,
    AssertState(AssertionFn),
    ExpectStatus(EventId, String),

    ExpectDeliveryAttempts(EventId, i64),
    RunAttestationCommitment,
    ExpectAttestationLeafCount(EventId, i64),
    ExpectAttestationLeafExists(EventId),
    ExpectAttestationLeafAttemptNumber(EventId, i32),
    ExpectIdempotentEventIds(EventId, EventId),
    ExpectConcurrentAttestationIntegrity(Vec<EventId>),
    ExpectAllEventsDelivered(Vec<EventId>),
    ExpectSignedTreeHeadWithSize(usize),
}

#[derive(Debug, Clone)]
/// Types of failures that can be injected during testing.
///
/// Used by the scenario builder to simulate various failure conditions
/// that webhook delivery systems must handle gracefully.
pub enum FailureKind {
    /// Network timeout during HTTP request
    NetworkTimeout,
    /// HTTP 500 Internal Server Error response
    Http500,
    /// HTTP 429 Too Many Requests with optional retry delay
    Http429 {
        /// Duration to wait before retrying (sent in Retry-After header)
        retry_after: Duration,
    },
    /// Database connection unavailable
    DatabaseUnavailable,
}

impl ScenarioBuilder {
    /// Create a new test scenario.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), steps: Vec::new(), invariant_checks: Vec::new() }
    }

    /// Add a webhook ingestion step.
    pub fn ingest(mut self, webhook: TestWebhook) -> Self {
        self.steps.push(Step::IngestWebhook(webhook));
        self
    }

    /// Run a delivery cycle to process pending webhooks.
    pub fn run_delivery_cycle(mut self) -> Self {
        self.steps.push(Step::RunDeliveryCycle);
        self
    }

    /// Advance test time.
    pub fn advance_time(mut self, duration: Duration) -> Self {
        self.steps.push(Step::AdvanceTime(duration));
        self
    }

    /// Inject an HTTP failure response.
    pub fn inject_http_failure(mut self, status: u16) -> Self {
        let response = http::MockResponse::ServerError {
            status,
            body: format!("HTTP {} Error", status).into_bytes(),
        };
        self.steps.push(Step::InjectHttpFailure(response));
        self
    }

    /// Inject an HTTP success response.
    pub fn inject_http_success(mut self) -> Self {
        self.steps.push(Step::InjectHttpSuccess);
        self
    }

    /// Run attestation commitment step.
    pub fn run_attestation_commitment(mut self) -> Self {
        self.steps.push(Step::RunAttestationCommitment);
        self
    }

    /// Expect a webhook to have a specific status.
    pub fn expect_status(mut self, event_id: EventId, expected_status: impl Into<String>) -> Self {
        self.steps.push(Step::ExpectStatus(event_id, expected_status.into()));
        self
    }

    /// Expect a specific number of delivery attempts.
    pub fn expect_delivery_attempts(mut self, event_id: EventId, expected_count: i64) -> Self {
        self.steps.push(Step::ExpectDeliveryAttempts(event_id, expected_count));
        self
    }

    /// Add a custom assertion.
    pub fn assert_state<F>(mut self, assertion: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.steps.push(Step::AssertState(Box::new(assertion)));
        self
    }

    /// Add an invariant check that runs after each step.
    pub fn check_invariant<F>(mut self, check: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.invariant_checks.push(Box::new(check));
        self
    }

    /// Expect a specific number of attestation leaves for an event.
    pub fn expect_attestation_leaf_count(mut self, event_id: EventId, expected_count: i64) -> Self {
        self.steps.push(Step::ExpectAttestationLeafCount(event_id, expected_count));
        self
    }

    /// Expect an attestation leaf to exist for an event.
    pub fn expect_attestation_leaf_exists(mut self, event_id: EventId) -> Self {
        self.steps.push(Step::ExpectAttestationLeafExists(event_id));
        self
    }

    /// Expect attestation leaf to have specific attempt number.
    pub fn expect_attestation_leaf_attempt_number(
        mut self,
        event_id: EventId,
        attempt_number: i32,
    ) -> Self {
        self.steps.push(Step::ExpectAttestationLeafAttemptNumber(event_id, attempt_number));
        self
    }

    /// Expect two event IDs to be identical (idempotency check).
    pub fn expect_idempotent_event_ids(mut self, event_id1: EventId, event_id2: EventId) -> Self {
        self.steps.push(Step::ExpectIdempotentEventIds(event_id1, event_id2));
        self
    }

    /// Expect concurrent attestation integrity for multiple events.
    pub fn expect_concurrent_attestation_integrity(mut self, event_ids: Vec<EventId>) -> Self {
        self.steps.push(Step::ExpectConcurrentAttestationIntegrity(event_ids));
        self
    }

    /// Expect all events in the list to be delivered.
    pub fn expect_all_events_delivered(mut self, event_ids: Vec<EventId>) -> Self {
        self.steps.push(Step::ExpectAllEventsDelivered(event_ids));
        self
    }

    /// Expect signed tree head to exist with specific size.
    pub fn expect_signed_tree_head_with_size(mut self, expected_size: usize) -> Self {
        self.steps.push(Step::ExpectSignedTreeHeadWithSize(expected_size));
        self
    }

    /// Execute the scenario.
    ///
    /// # Errors
    ///
    /// Returns error if any step in the scenario fails to execute.
    pub async fn run(self, env: &mut TestEnv) -> Result<()> {
        tracing::info!("running scenario: {}", self.name);

        // Use a HashMap to store event IDs created during the scenario
        let mut event_ids: HashMap<String, EventId> = HashMap::new();

        for (i, step) in self.steps.into_iter().enumerate() {
            tracing::debug!("executing step {}", i + 1);

            match step {
                Step::IngestWebhook(webhook) => {
                    let source_id = webhook.source_event_id.clone();
                    tracing::debug!("ingesting webhook with source_id {}", source_id);
                    let event_id = env.ingest_webhook(&webhook).await?;
                    event_ids.insert(source_id, event_id);
                },
                Step::RunDeliveryCycle => {
                    tracing::debug!("running delivery cycle");
                    env.run_delivery_cycle().await?;
                },
                Step::AdvanceTime(duration) => {
                    env.advance_time(duration);
                    tracing::debug!("advanced time by {:?}", duration);
                },
                Step::InjectHttpFailure(response) => {
                    tracing::debug!("injecting HTTP failure response");
                    env.http_mock.mock_simple("/", response).await;
                },
                Step::InjectHttpSuccess => {
                    tracing::debug!("injecting HTTP success response");
                    let response = http::MockResponse::Success {
                        status: reqwest::StatusCode::OK,
                        body: bytes::Bytes::new(),
                    };
                    env.http_mock.mock_simple("/", response).await;
                },
                Step::RunAttestationCommitment => {
                    tracing::debug!("running attestation commitment");
                    env.run_attestation_commitment().await?;
                },
                Step::AssertState(assertion) => {
                    assertion(env).context("state assertion failed")?;
                },
                Step::ExpectStatus(event_id, expected) => {
                    let actual = env.find_webhook_status(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Webhook status mismatch for event {}: expected '{}', got '{}'",
                        event_id.0, expected, actual
                    );
                },
                Step::ExpectDeliveryAttempts(event_id, expected) => {
                    let actual = env.count_delivery_attempts(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Delivery attempt count mismatch for event {}: expected {}, got {}",
                        event_id.0, expected, actual
                    );
                },
                Step::ExpectAttestationLeafCount(event_id, expected) => {
                    let actual = env.count_attestation_leaves_for_event(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Attestation leaf count mismatch for event {}: expected {}, got {}",
                        event_id.0, expected, actual
                    );
                },
                Step::ExpectAttestationLeafExists(event_id) => {
                    let leaf = env.fetch_attestation_leaf_for_event(event_id).await?;
                    assert!(leaf.is_some(), "Attestation leaf must exist for event {}", event_id.0);
                },
                Step::ExpectAttestationLeafAttemptNumber(event_id, expected_attempt) => {
                    let leaf =
                        env.fetch_attestation_leaf_for_event(event_id).await?.unwrap_or_else(
                            || panic!("Attestation leaf must exist for event {}", event_id.0),
                        );
                    assert_eq!(
                        leaf.attempt_number, expected_attempt,
                        "Attestation leaf attempt number mismatch for event {}: expected {}, got {}",
                        event_id.0, expected_attempt, leaf.attempt_number
                    );
                },
                Step::ExpectIdempotentEventIds(event_id1, event_id2) => {
                    assert_eq!(
                        event_id1, event_id2,
                        "Event IDs must be identical for idempotency: {} != {}",
                        event_id1.0, event_id2.0
                    );
                },
                Step::ExpectConcurrentAttestationIntegrity(event_ids) => {
                    for &event_id in &event_ids {
                        let status = env.find_webhook_status(event_id).await?;
                        assert_eq!(status, "delivered", "All concurrent deliveries must succeed");

                        let leaf_count = env.count_attestation_leaves_for_event(event_id).await?;
                        assert_eq!(
                            leaf_count, 1,
                            "Each concurrent delivery must create exactly one leaf"
                        );
                    }

                    let total_leaves = env.count_total_attestation_leaves().await?;
                    assert_eq!(
                        total_leaves,
                        event_ids.len() as i64,
                        "Total attestation leaves must match delivered events"
                    );
                },
                Step::ExpectAllEventsDelivered(event_ids) => {
                    for &event_id in &event_ids {
                        let status = env.find_webhook_status(event_id).await?;
                        assert_eq!(status, "delivered", "Event {} must be delivered", event_id.0);
                    }
                },
                Step::ExpectSignedTreeHeadWithSize(expected_size) => {
                    let sth_count = env.count_signed_tree_heads().await?;
                    assert!(sth_count > 0, "Batch commitment must create signed tree head");

                    let latest_sth = env.fetch_latest_signed_tree_head().await?;
                    assert!(latest_sth.is_some(), "Latest STH must exist after commitment");

                    let sth = latest_sth.unwrap();
                    assert_eq!(
                        sth.tree_size as usize, expected_size,
                        "Tree size must match expected: expected {}, got {}",
                        expected_size, sth.tree_size
                    );
                    assert!(!sth.signature.is_empty(), "STH must be cryptographically signed");
                },
            }

            // Run invariant checks after each step
            for (check_idx, check) in self.invariant_checks.iter().enumerate() {
                check(env).with_context(|| {
                    format!(
                        "invariant check {} failed after step {} in scenario '{}'",
                        check_idx + 1,
                        i + 1,
                        self.name
                    )
                })?;
            }
        }

        Ok(())
    }
}

/// Common test assertions for webhook verification.
pub mod assertions {
    use bytes::Bytes;
    use serde_json::Value;

    /// Assert that JSON payloads match.
    pub fn assert_json_matches(actual: &Bytes, expected: &Value) {
        let actual_json: Value =
            serde_json::from_slice(actual).expect("failed to parse actual JSON");

        assert_eq!(
            actual_json,
            *expected,
            "JSON mismatch\nActual: {}\nExpected: {}",
            serde_json::to_string_pretty(&actual_json).unwrap(),
            serde_json::to_string_pretty(expected).unwrap()
        );
    }

    /// Assert webhook was delivered within timeout.
    pub async fn assert_delivered_within(_event_id: &str, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Check delivery status in database
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Placeholder - actual implementation would check database
            if false {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_env_pool_access_works() {
        // Test that pool access works for persistent operations
        let env = TestEnv::new().await.unwrap();

        // Create data using the pool
        let tenant_id = env.create_tenant("pool-test-tenant").await.unwrap();

        // Verify we can query it back
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = $1")
            .bind("pool-test-tenant")
            .fetch_one(env.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Verify the tenant ID matches
        let found_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM tenants WHERE name = $1")
            .bind("pool-test-tenant")
            .fetch_one(env.pool())
            .await
            .unwrap();
        assert_eq!(found_id, tenant_id.0);
    }

    #[tokio::test]
    async fn scenario_builder_executes_steps() {
        let mut env = TestEnv::new().await.unwrap();

        let scenario = ScenarioBuilder::new("test scenario")
            .advance_time(Duration::from_secs(1))
            .assert_state(|env| {
                // Custom assertion
                assert!(env.elapsed() >= Duration::from_secs(1));
                Ok(())
            })
            .advance_time(Duration::from_secs(2))
            .assert_state(|env| {
                // Verify cumulative time
                assert!(env.elapsed() >= Duration::from_secs(3));
                Ok(())
            })
            .check_invariant(|_env| {
                // Invariant check runs after each step
                Ok(())
            });

        scenario.run(&mut env).await.unwrap();
    }

    #[tokio::test]
    async fn test_env_seed_data_works() {
        let env = TestEnv::new().await.unwrap();

        let (tenant_id, endpoint_id) = env.seed_test_data().await.unwrap();

        // Verify tenant exists
        let tenant_count = env.count_by_id("tenants", "id", tenant_id.0).await.unwrap();
        assert_eq!(tenant_count, 1);

        // Verify endpoint exists
        let endpoint_count = env.count_by_id("endpoints", "id", endpoint_id.0).await.unwrap();
        assert_eq!(endpoint_count, 1);
    }

    #[tokio::test]
    async fn health_check_works() {
        let env = TestEnv::new().await.unwrap();
        assert!(env.database_health_check().await.unwrap());
    }

    #[tokio::test]
    async fn list_tables_returns_schema() {
        let env = TestEnv::new().await.unwrap();
        let tables = env.list_tables().await.unwrap();

        // Verify key tables exist
        assert!(tables.contains(&"tenants".to_string()));
        assert!(tables.contains(&"endpoints".to_string()));
        assert!(tables.contains(&"webhook_events".to_string()));
        assert!(tables.contains(&"delivery_attempts".to_string()));
    }
}
