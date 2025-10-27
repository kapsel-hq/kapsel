//! Scenario builder for complex multi-step integration tests.
//!
//! Provides a DSL for constructing deterministic test scenarios with
//! time control, invariant validation, and assertion steps.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use kapsel_core::models::EventStatus;

use crate::{http, AssertionFn, EventId, InvariantCheckFn, TestEnv, TestWebhook};

/// Test scenario builder for complex multi-step tests.
///
/// Enables deterministic testing with time control and invariant validation.
pub struct ScenarioBuilder {
    name: String,
    steps: Vec<Step>,
    baseline_tree_size: Option<i64>,
    invariant_checks: Vec<InvariantCheckFn>,
}

enum Step {
    IngestWebhook(TestWebhook),
    RunDeliveryCycle,
    AdvanceTime(Duration),
    InjectHttpFailure(http::MockResponse),
    InjectHttpSuccess,
    AssertState(AssertionFn),
    ExpectStatus(EventId, EventStatus),
    ExpectDeliveryAttempts(EventId, i64),
    RunAttestationCommitment,
    ExpectAttestationLeafCount(EventId, i64),
    ExpectAttestationLeafExists(EventId),
    ExpectAttestationLeafAttemptNumber(EventId, i32),
    ExpectIdempotentEventIds(EventId, EventId),
    ExpectConcurrentAttestationIntegrity(Vec<EventId>),
    ExpectAllEventsDelivered(Vec<EventId>),
    ExpectSignedTreeHeadWithSize(usize),
    CaptureTreeSize,
    SnapshotEventsTable(String),
    SnapshotDeliveryAttempts(String),
    SnapshotDatabaseSchema(String),
    SnapshotTable(String, String, String), // snapshot_name, table_name, order_by
}

/// Types of failures that can be injected during testing.
///
/// Used by the scenario builder to simulate various failure conditions
/// that webhook delivery systems must handle gracefully.
#[derive(Debug, Clone)]
pub enum FailureKind {
    /// Network timeout during HTTP request
    NetworkTimeout,
    /// HTTP 500 Internal Server Error response
    Http500,
    /// HTTP 502 Bad Gateway (upstream server error)
    Http502,
    /// HTTP 503 Service Unavailable (temporary overload)
    Http503,
    /// HTTP 504 Gateway Timeout (upstream timeout)
    Http504,
    /// HTTP 429 Too Many Requests with optional retry delay
    Http429 {
        /// Duration to wait before retrying (sent in Retry-After header)
        retry_after: Option<u64>,
    },
    /// Connection reset by peer (network-level failure)
    ConnectionReset,
    /// DNS resolution failure (can't resolve endpoint hostname)
    DnsResolutionFailed,
    /// SSL/TLS certificate validation failure
    SslCertificateInvalid,
    /// Very slow response (not timeout, but takes excessive time)
    SlowResponse {
        /// Response delay in seconds
        delay_seconds: u32,
    },
    /// Intermittent failure (succeeds/fails randomly)
    IntermittentFailure {
        /// Probability of failure (0.0 = never fail, 1.0 = always fail)
        failure_rate: f32,
    },
    /// Database connection unavailable
    DatabaseUnavailable,
    /// Database query timeout (connection exists but queries hang)
    DatabaseTimeout,
    /// Memory pressure simulation (out of memory)
    MemoryPressure,
    /// Disk space exhaustion
    DiskSpaceFull,
    /// Network partition (complete network isolation)
    NetworkPartition {
        /// Duration of the partition
        duration_seconds: u32,
    },
}

impl ScenarioBuilder {
    /// Create a new test scenario.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            baseline_tree_size: None,
            invariant_checks: Vec::new(),
        }
    }

    /// Add a webhook ingestion step.
    #[must_use]
    pub fn ingest(mut self, webhook: TestWebhook) -> Self {
        self.steps.push(Step::IngestWebhook(webhook));
        self
    }

    /// Run a delivery cycle to process pending webhooks.
    #[must_use]
    pub fn run_delivery_cycle(mut self) -> Self {
        self.steps.push(Step::RunDeliveryCycle);
        self
    }

    /// Advance test time.
    #[must_use]
    pub fn advance_time(mut self, duration: Duration) -> Self {
        self.steps.push(Step::AdvanceTime(duration));
        self
    }

    /// Inject an HTTP failure response.
    #[must_use]
    pub fn inject_http_failure(mut self, status: u16) -> Self {
        let response = http::MockResponse::ServerError {
            status,
            body: format!("HTTP {status} Error").into_bytes(),
        };
        self.steps.push(Step::InjectHttpFailure(response));
        self
    }

    /// Inject HTTP success response for all subsequent requests.
    #[must_use]
    pub fn inject_http_success(mut self) -> Self {
        self.steps.push(Step::InjectHttpSuccess);
        self
    }

    /// Run attestation commitment step.
    #[must_use]
    pub fn run_attestation_commitment(mut self) -> Self {
        self.steps.push(Step::RunAttestationCommitment);
        self
    }

    /// Expect a webhook to have a specific status.
    #[must_use]
    pub fn expect_status(mut self, event_id: EventId, expected_status: EventStatus) -> Self {
        self.steps.push(Step::ExpectStatus(event_id, expected_status));
        self
    }

    /// Expect a specific number of delivery attempts.
    #[must_use]
    pub fn expect_delivery_attempts(mut self, event_id: EventId, expected: i32) -> Self {
        self.steps.push(Step::ExpectDeliveryAttempts(event_id, expected.into()));
        self
    }

    /// Add a custom assertion.
    #[must_use]
    pub fn assert_state<F>(mut self, assertion: F) -> Self
    where
        F: Fn(&mut TestEnv) -> Result<()> + 'static,
    {
        self.steps.push(Step::AssertState(Box::new(assertion)));
        self
    }

    /// Add an invariant check that runs after each step.
    ///
    /// Invariant checks are executed after every scenario step to ensure
    /// system correctness properties hold throughout the entire scenario.
    /// This catches violations immediately when they occur.
    #[must_use]
    pub fn check_invariant<F>(mut self, check: F) -> Self
    where
        F: for<'a> Fn(
                &'a mut TestEnv,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
            > + 'static,
    {
        self.invariant_checks.push(Box::new(check));
        self
    }

    /// Add common invariant: no events are lost during processing.
    #[must_use]
    pub fn check_no_data_loss(self) -> Self {
        self.check_invariant(|env| {
            Box::pin(async move {
                // All events should be in terminal states, processing, or pending
                let total_events = env.count_total_events().await?;
                let terminal_events = env.count_terminal_events().await?;
                let processing_events = env.count_processing_events().await?;
                let pending_events = env.count_pending_events().await?;

                anyhow::ensure!(
                    total_events == terminal_events + processing_events + pending_events,
                    "Data loss detected: {total_events} total, {terminal_events} terminal, {processing_events} processing, {pending_events} pending"
                );
                Ok(())
            })
        })
    }

    /// Add common invariant: retry attempts never exceed configured maximum.
    #[must_use]
    pub fn check_retry_bounds(self, max_retries: u32) -> Self {
        self.check_invariant(move |env| {
            Box::pin(async move {
                let events = env.get_all_events().await?;
                for event in events {
                    anyhow::ensure!(
                        event.attempt_count() <= max_retries + 1,
                        "Event {} exceeded max retries: {} > {}",
                        event.id.0,
                        event.attempt_count(),
                        max_retries + 1
                    );
                }
                Ok(())
            })
        })
    }

    /// Add common invariant: events maintain proper state transitions.
    #[must_use]
    pub fn check_state_machine_integrity(self) -> Self {
        self.check_invariant(|env| {
            Box::pin(async move {
                let events = env.get_all_events().await?;
                for event in events {
                    // Verify terminal states don't have future retries scheduled
                    if matches!(
                        event.status,
                        EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter
                    ) {
                        anyhow::ensure!(
                            event.next_retry_at.is_none(),
                            "Terminal event {} has scheduled retry",
                            event.id.0
                        );
                    }
                }
                Ok(())
            })
        })
    }

    /// Expect a specific number of attestation leaves for an event.
    #[must_use]
    pub fn expect_attestation_leaf_count(mut self, event_id: EventId, expected_count: i64) -> Self {
        self.steps.push(Step::ExpectAttestationLeafCount(event_id, expected_count));
        self
    }

    /// Expect an attestation leaf to exist for an event.
    #[must_use]
    pub fn expect_attestation_leaf_exists(mut self, event_id: EventId) -> Self {
        self.steps.push(Step::ExpectAttestationLeafExists(event_id));
        self
    }

    /// Expect attestation leaf to have specific attempt number.
    #[must_use]
    pub fn expect_attestation_leaf_attempt_number(
        mut self,
        event_id: EventId,
        attempt_number: i32,
    ) -> Self {
        self.steps.push(Step::ExpectAttestationLeafAttemptNumber(event_id, attempt_number));
        self
    }

    /// Expect two event IDs to be identical (idempotency check).
    #[must_use]
    pub fn expect_idempotent_event_ids(mut self, event_id1: EventId, event_id2: EventId) -> Self {
        self.steps.push(Step::ExpectIdempotentEventIds(event_id1, event_id2));
        self
    }

    /// Expect concurrent attestation integrity for multiple events.
    #[must_use]
    pub fn expect_concurrent_attestation_integrity(mut self, event_ids: Vec<EventId>) -> Self {
        self.steps.push(Step::ExpectConcurrentAttestationIntegrity(event_ids));
        self
    }

    /// Expect all events in the list to be delivered.
    #[must_use]
    pub fn expect_all_events_delivered(mut self, event_ids: Vec<EventId>) -> Self {
        self.steps.push(Step::ExpectAllEventsDelivered(event_ids));
        self
    }

    /// Expect signed tree head to grow by specific amount after commitment.
    #[must_use]
    pub fn expect_signed_tree_head_with_size(mut self, growth: usize) -> Self {
        self.steps.push(Step::ExpectSignedTreeHeadWithSize(growth));
        self
    }

    /// Capture current tree size as baseline for growth measurement.
    #[must_use]
    pub fn capture_tree_size(mut self) -> Self {
        self.steps.push(Step::CaptureTreeSize);
        self
    }

    /// Capture a snapshot of the events table state for regression testing.
    ///
    /// Creates an insta snapshot that will catch changes to event processing
    /// behavior across system boundaries.
    #[must_use]
    pub fn snapshot_events_table(mut self, snapshot_name: impl Into<String>) -> Self {
        self.steps.push(Step::SnapshotEventsTable(snapshot_name.into()));
        self
    }

    /// Capture a snapshot of delivery attempts for debugging.
    ///
    /// Useful for verifying retry behavior and delivery attempt patterns.
    #[must_use]
    pub fn snapshot_delivery_attempts(mut self, snapshot_name: impl Into<String>) -> Self {
        self.steps.push(Step::SnapshotDeliveryAttempts(snapshot_name.into()));
        self
    }

    /// Capture database schema snapshot to detect schema evolution issues.
    ///
    /// Critical for catching unintended schema changes in pull requests.
    #[must_use]
    pub fn snapshot_database_schema(mut self, snapshot_name: impl Into<String>) -> Self {
        self.steps.push(Step::SnapshotDatabaseSchema(snapshot_name.into()));
        self
    }

    /// Capture snapshot of any database table.
    ///
    /// Generic method for snapshotting lookup tables, configuration, etc.
    #[must_use]
    pub fn snapshot_table(
        mut self,
        snapshot_name: impl Into<String>,
        table_name: impl Into<String>,
        order_by: impl Into<String>,
    ) -> Self {
        self.steps.push(Step::SnapshotTable(
            snapshot_name.into(),
            table_name.into(),
            order_by.into(),
        ));
        self
    }

    /// Execute the scenario.
    ///
    /// # Errors
    ///
    /// Returns error if any step in the scenario fails to execute.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::future_not_send)]
    pub async fn run(mut self, env: &mut TestEnv) -> Result<()> {
        tracing::info!("running scenario: {}", self.name);

        // Use a HashMap to store event IDs created during the scenario
        #[allow(clippy::collection_is_never_read)]
        let mut event_ids: HashMap<String, EventId> = HashMap::new();

        for (i, step) in self.steps.into_iter().enumerate() {
            tracing::debug!("executing step {}", i + 1);

            // Execute the step
            match step {
                Step::IngestWebhook(webhook) => {
                    let source_id = webhook.source_event_id.clone();
                    tracing::debug!("ingesting webhook with source_id {}", source_id);
                    let event_id = env.ingest_webhook(&webhook).await?;
                    event_ids.insert(source_id, event_id);
                },
                Step::RunDeliveryCycle => {
                    tracing::debug!("running test-isolated delivery cycle");
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
                    let actual = env.event_status(event_id).await?;
                    assert_eq!(
                        actual, expected,
                        "Webhook status mismatch for event {}: expected '{:?}', got '{:?}'",
                        event_id.0, expected, actual
                    );
                },
                Step::ExpectDeliveryAttempts(event_id, expected) => {
                    let actual = env.count_delivery_attempts(event_id).await?;
                    assert_eq!(
                        actual,
                        u32::try_from(expected).unwrap_or(u32::MAX),
                        "Delivery attempt count mismatch for event {}: expected {}, got {}",
                        event_id.0,
                        expected,
                        actual
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
                            #[allow(clippy::panic)]
                            || panic!("Attestation leaf must exist for event {}", event_id.0),
                        );
                    assert_eq!(
                        leaf.attempt_number, expected_attempt,
                        "Attestation leaf attempt number mismatch for event {}: expected {}, got {}",
                        event_id.0, expected_attempt, leaf.attempt_number
                    );
                },
                Step::ExpectIdempotentEventIds(first_event_id, second_event_id) => {
                    assert_eq!(
                        first_event_id, second_event_id,
                        "Event IDs must be identical for idempotency: {first_event_id} != {second_event_id}"
                    );
                },
                Step::ExpectConcurrentAttestationIntegrity(event_ids) => {
                    // First ensure all pending leaves are committed to database
                    if env.attestation_service.is_some() {
                        let merkle_service = env.attestation_service.as_ref().ok_or_else(|| {
                            anyhow::anyhow!(
                                "attestation service should be configured for this test"
                            )
                        })?;
                        let pending_count = merkle_service.read().await.pending_count();
                        if pending_count > 0 {
                            tracing::debug!(
                                "committing {} pending attestation leaves before integrity check",
                                pending_count
                            );
                            env.run_attestation_commitment().await?;
                        }
                    }

                    for &event_id in &event_ids {
                        let leaf_count = env.count_attestation_leaves_for_event(event_id).await?;
                        assert_eq!(
                            leaf_count, 1,
                            "Each concurrent delivery must create exactly one leaf"
                        );
                    }

                    // Count leaves specifically for these events, not all leaves in database
                    let mut event_leaves_total = 0i64;
                    for &event_id in &event_ids {
                        event_leaves_total +=
                            env.count_attestation_leaves_for_event(event_id).await?;
                    }
                    assert_eq!(
                        event_leaves_total,
                        i64::try_from(event_ids.len()).unwrap_or(0),
                        "Total attestation leaves must match delivered events"
                    );
                },
                Step::CaptureTreeSize => {
                    // Get current tree size as baseline for growth measurement
                    let current_size = env.get_current_tree_size().await?;
                    self.baseline_tree_size = Some(current_size);
                    tracing::debug!("captured baseline tree size: {}", current_size);
                },
                Step::ExpectAllEventsDelivered(event_ids) => {
                    for &event_id in &event_ids {
                        let status = env.event_status(event_id).await?;
                        assert_eq!(
                            status,
                            EventStatus::Delivered,
                            "Event {} must be delivered",
                            event_id.0
                        );
                    }
                },
                Step::ExpectSignedTreeHeadWithSize(expected_size) => {
                    let sth_count = env.count_signed_tree_heads().await?;
                    assert!(sth_count > 0, "Batch commitment must create signed tree head");

                    let latest_sth = env.get_latest_signed_tree_head().await?;
                    assert!(latest_sth.is_some(), "Latest STH must exist after commitment");

                    #[allow(clippy::unwrap_used)]
                    let sth = latest_sth.unwrap();

                    if let Some(baseline) = self.baseline_tree_size {
                        let actual_growth = sth.tree_size - baseline;
                        assert!(
                            usize::try_from(actual_growth).unwrap_or(0) >= expected_size,
                            "Tree size growth must be at least expected: expected {expected_size}, got {actual_growth}"
                        );
                    } else {
                        // Fallback to absolute size check if no baseline captured
                        assert_eq!(
                            usize::try_from(sth.tree_size).unwrap_or(0),
                            expected_size,
                            "Tree size must match expected: expected {}, got {}",
                            expected_size,
                            sth.tree_size
                        );
                    }
                    assert!(!sth.signature.is_empty(), "STH must be cryptographically signed");
                },
                Step::SnapshotEventsTable(snapshot_name) => {
                    let snapshot = env.snapshot_events_table().await?;
                    insta::assert_snapshot!(snapshot_name, snapshot);
                },
                Step::SnapshotDeliveryAttempts(snapshot_name) => {
                    let snapshot = env.snapshot_delivery_attempts().await?;
                    insta::assert_snapshot!(snapshot_name, snapshot);
                },
                Step::SnapshotDatabaseSchema(snapshot_name) => {
                    let snapshot = env.snapshot_database_schema().await?;
                    insta::assert_snapshot!(snapshot_name, snapshot);
                },
                Step::SnapshotTable(snapshot_name, table_name, order_by) => {
                    let snapshot = env.snapshot_table(&table_name, &order_by).await?;
                    insta::assert_snapshot!(snapshot_name, snapshot);
                },
            }

            // Run invariant checks after each step
            for (check_idx, check) in self.invariant_checks.iter().enumerate() {
                check(env).await.context(format!(
                    "invariant check {} failed after step {} in scenario '{}'",
                    check_idx + 1,
                    i + 1,
                    self.name
                ))?;
            }
        }

        Ok(())
    }
}
