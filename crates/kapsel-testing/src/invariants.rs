//! Invariant checking and property-based testing utilities.
//!
//! Provides tools to verify critical system invariants and reliability
//! guarantees hold across different system states and inputs.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use kapsel_core::EventStatus;
use uuid::Uuid;

/// Core system invariants that must always hold.
pub struct Invariants;

impl Invariants {
    /// At-least-once delivery: Every accepted webhook is eventually delivered
    /// or marked failed.
    pub fn at_least_once_delivery(events: &[WebhookEvent]) -> Result<()> {
        for event in events {
            ensure!(
                event.is_terminal_state(),
                "Event {} is neither delivered nor failed after max retries",
                event.id
            );
        }
        Ok(())
    }

    /// Idempotency: Duplicate webhooks produce identical responses.
    pub fn idempotency_guarantee(
        original: &WebhookResponse,
        duplicate: &WebhookResponse,
    ) -> Result<()> {
        ensure!(
            original.event_id == duplicate.event_id,
            "Duplicate webhook created new event: {} != {}",
            original.event_id,
            duplicate.event_id
        );
        ensure!(
            original.status_code == duplicate.status_code,
            "Duplicate webhook returned different status: {} != {}",
            original.status_code,
            duplicate.status_code
        );
        Ok(())
    }

    /// Retry bounds: Attempts never exceed configured maximum.
    pub fn retry_count_bounded(event: &WebhookEvent, max_retries: u32) -> Result<()> {
        ensure!(
            event.attempt_count <= max_retries + 1, // +1 for initial attempt
            "Event {} exceeded max retries: {} > {}",
            event.id,
            event.attempt_count,
            max_retries + 1
        );
        Ok(())
    }

    /// Exponential backoff: Retry delays increase exponentially with jitter.
    pub fn exponential_backoff_timing(attempts: &[DeliveryAttempt]) -> Result<()> {
        if attempts.len() < 2 {
            return Ok(());
        }

        for window in attempts.windows(2) {
            let delay = window[1].attempted_at - window[0].attempted_at;
            let expected_min_secs = 2_i64.pow(window[0].attempt_number);
            let expected_min = chrono::Duration::seconds(expected_min_secs);
            let expected_max = chrono::Duration::seconds(expected_min_secs * 3); // Allow for jitter

            ensure!(
                delay >= expected_min * 3 / 4, // 25% jitter down
                "Backoff too short at attempt {}: {:?} < {:?}",
                window[0].attempt_number,
                delay,
                expected_min * 3 / 4
            );
            ensure!(
                delay <= expected_max,
                "Backoff too long at attempt {}: {:?} > {:?}",
                window[0].attempt_number,
                delay,
                expected_max
            );
        }
        Ok(())
    }

    /// Circuit breaker correctness: Opens and closes at correct thresholds.
    pub fn circuit_breaker_state_transitions(history: &CircuitBreakerHistory) -> Result<()> {
        let mut consecutive_failures = 0;
        let mut state = CircuitState::Closed;

        for (event, success) in &history.events {
            match state {
                CircuitState::Closed => {
                    if *success {
                        consecutive_failures = 0;
                    } else {
                        consecutive_failures += 1;
                        if consecutive_failures >= history.open_threshold {
                            state = CircuitState::Open;
                            ensure!(
                                event.circuit_state == CircuitState::Open,
                                "Circuit should be open after {consecutive_failures} failures"
                            );
                        }
                    }
                },
                CircuitState::Open => {
                    // Should reject requests immediately
                    ensure!(!event.made_request, "Circuit open but request was made");
                },
                CircuitState::HalfOpen => {
                    if *success {
                        state = CircuitState::Closed;
                    } else {
                        state = CircuitState::Open;
                    }
                },
            }
        }
        Ok(())
    }

    /// Multi-tenancy isolation: No data leakage between tenants.
    pub fn tenant_isolation(
        first_tenant_data: &[WebhookEvent],
        second_tenant_data: &[WebhookEvent],
        first_tenant_id: Uuid,
        second_tenant_id: Uuid,
    ) -> Result<()> {
        // No events from A in B's data
        for event in second_tenant_data {
            ensure!(
                event.tenant_id != first_tenant_id,
                "Tenant A event {} found in Tenant B data",
                event.id
            );
        }

        // No events from B in A's data
        for event in first_tenant_data {
            ensure!(
                event.tenant_id != second_tenant_id,
                "Tenant B event {} found in Tenant A data",
                event.id
            );
        }

        Ok(())
    }

    /// Ordering guarantee: Events processed in FIFO order per endpoint.
    pub fn fifo_ordering_per_endpoint(events: &[WebhookEvent]) -> Result<()> {
        let mut endpoint_sequences: HashMap<Uuid, Vec<&WebhookEvent>> = HashMap::new();

        for event in events {
            endpoint_sequences.entry(event.endpoint_id).or_default().push(event);
        }

        for (endpoint_id, sequence) in endpoint_sequences {
            for window in sequence.windows(2) {
                ensure!(
                    window[0].received_at <= window[1].received_at,
                    "FIFO violation for endpoint {}: event {} after {}",
                    endpoint_id,
                    window[0].id,
                    window[1].id
                );

                if window[0].status == EventStatus::Delivered
                    && window[1].status == EventStatus::Delivered
                {
                    ensure!(
                        window[0].delivered_at <= window[1].delivered_at,
                        "Delivery order violation for endpoint {endpoint_id}"
                    );
                }
            }
        }

        Ok(())
    }

    /// No lost events: Every ingested event exists in exactly one state.
    pub fn no_lost_events<S: ::std::hash::BuildHasher, T: ::std::hash::BuildHasher>(
        ingested: &HashSet<Uuid, S>,
        current_state: &HashMap<Uuid, WebhookEvent, T>,
    ) -> Result<()> {
        for event_id in ingested {
            ensure!(
                current_state.contains_key(event_id),
                "Event {event_id} was ingested but not found in current state"
            );
        }

        for event_id in current_state.keys() {
            ensure!(ingested.contains(event_id), "Event {event_id} exists but was never ingested");
        }

        Ok(())
    }

    /// Audit completeness: Every state transition is recorded.
    pub fn audit_trail_complete(event: &WebhookEvent, audit_entries: &[AuditEntry]) -> Result<()> {
        // We should have entry for each state transition
        let expected_entries = match event.status {
            EventStatus::Received => 1,
            EventStatus::Pending => 2,
            EventStatus::Delivering => 2 + event.attempt_count as usize,
            EventStatus::Delivered | EventStatus::Failed | EventStatus::DeadLetter => {
                3 + event.attempt_count as usize
            },
        };

        ensure!(
            audit_entries.len() >= expected_entries,
            "Missing audit entries for event {}: have {}, expected at least {}",
            event.id,
            audit_entries.len(),
            expected_entries
        );

        // Verify chronological order
        for window in audit_entries.windows(2) {
            ensure!(
                window[0].timestamp <= window[1].timestamp,
                "Audit entries out of order for event {}",
                event.id
            );
        }

        Ok(())
    }

    /// Rate limit compliance: Never exceeds configured limits.
    pub fn rate_limit_compliance(
        requests: &[RequestRecord],
        limit: usize,
        window: Duration,
    ) -> Result<()> {
        for i in 0..requests.len() {
            let window_start = requests[i].timestamp;
            let window_end = window_start + window;

            let count = requests[i..].iter().take_while(|r| r.timestamp < window_end).count();

            ensure!(
                count <= limit,
                "Rate limit exceeded at {}: {} requests in {:?} window (limit: {})",
                window_start.format("%H:%M:%S%.3f"),
                count,
                window,
                limit
            );
        }

        Ok(())
    }
}

/// Test data structures for invariant testing.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    /// Unique identifier for this webhook event
    pub id: Uuid,
    /// Tenant that owns this webhook event
    pub tenant_id: Uuid,
    /// Target endpoint for webhook delivery
    pub endpoint_id: Uuid,
    /// External source identifier for idempotency tracking
    pub source_event_id: String,
    /// Strategy used for duplicate detection and deduplication
    pub idempotency_strategy: String,
    /// Current processing status of this webhook event
    pub status: EventStatus,
    /// Number of delivery attempts made so far
    pub attempt_count: u32,
    /// Timestamp for next retry attempt (if scheduled for retry)
    pub next_retry_at: Option<chrono::DateTime<chrono::Utc>>,
    /// HTTP headers to include in webhook request
    pub headers: HashMap<String, String>,
    /// Webhook payload body as raw bytes
    pub body: bytes::Bytes,
    /// MIME type of the payload body
    pub content_type: String,
    /// Timestamp when this event was first received
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp when event was successfully delivered (if completed)
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl WebhookEvent {
    /// Returns true if this event is in a terminal state (delivered or failed).
    pub fn is_terminal_state(&self) -> bool {
        matches!(self.status, EventStatus::Delivered | EventStatus::Failed)
    }
}

#[derive(Debug, Clone)]
/// Response information from a webhook delivery attempt.
///
/// Captures the essential information about how the target endpoint
/// responded to a webhook delivery request.
pub struct WebhookResponse {
    /// ID of the webhook event that was delivered
    pub event_id: Uuid,
    /// HTTP status code returned by the target endpoint
    pub status_code: u16,
}

#[derive(Debug, Clone)]
/// Information about a single webhook delivery attempt.
///
/// Records the details of each attempt to deliver a webhook to a target
/// endpoint, including timing and response information.
pub struct DeliveryAttempt {
    /// Sequential attempt number (1-based) for this delivery
    pub attempt_number: u32,
    /// Timestamp when this delivery attempt was made
    pub attempted_at: chrono::DateTime<chrono::Utc>,
    /// HTTP status code from endpoint response (None if request failed)
    pub response_status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// State of a circuit breaker protecting an endpoint.
///
/// Circuit breakers prevent cascading failures by temporarily stopping
/// requests to endpoints that are experiencing issues.
pub enum CircuitState {
    /// Circuit is closed - requests are allowed through normally
    Closed,
    /// Circuit is open - requests are rejected to protect the endpoint
    Open,
    /// Circuit is half-open - limited requests allowed to test recovery
    HalfOpen,
}

#[derive(Debug)]
/// History of circuit breaker events for testing and validation.
///
/// Tracks the sequence of circuit breaker state changes and outcomes
/// to verify proper circuit breaker behavior during testing.
pub struct CircuitBreakerHistory {
    /// Sequence of circuit events and their success/failure outcomes
    pub events: Vec<(CircuitEvent, bool)>,
    /// Number of failures required to trip the circuit breaker open
    pub open_threshold: usize,
}

#[derive(Debug)]
/// A single event in the circuit breaker's operational history.
///
/// Records state transitions and outcomes for circuit breaker testing
/// and invariant validation.
pub struct CircuitEvent {
    /// State of the circuit breaker when this event occurred
    pub circuit_state: CircuitState,
    /// Whether a request was actually made during this circuit event
    pub made_request: bool,
}

/// Record of a single audit trail entry for a webhook event.
///
/// Audit entries track state transitions and actions taken on webhook
/// events to ensure complete traceability and compliance verification.
#[derive(Debug)]
pub struct AuditEntry {
    /// ID of the webhook event this audit entry is for
    pub event_id: Uuid,
    /// Timestamp when this audit action occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Description of the action that was performed
    pub action: String,
}

/// Record of a single HTTP request made to an endpoint.
///
/// Used for testing rate limiting, timing analysis, and request
/// tracking across the webhook delivery system.
#[derive(Debug)]
pub struct RequestRecord {
    /// Timestamp when this request was made
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// ID of the endpoint that received this request
    pub endpoint_id: Uuid,
}

/// Test scenario for verifying retry behavior and backoff strategies.
///
/// Encapsulates the parameters and expected outcomes for testing
/// retry logic, exponential backoff, and failure handling.
#[derive(Debug)]
pub struct RetryScenario {
    /// Maximum number of retry attempts allowed in this scenario
    pub max_attempts: u32,
    /// Sequence of success/failure outcomes for each attempt
    pub outcomes: Vec<bool>,
    /// Total elapsed time for the complete retry sequence
    pub total_duration: Duration,
}

/// Test scenario for verifying circuit breaker behavior.
///
/// Defines parameters and request outcomes for testing circuit breaker
/// state transitions, threshold detection, and failure protection.
#[derive(Debug)]
pub struct CircuitBreakerScenario {
    /// Number of consecutive failures required to open the circuit
    pub open_threshold: usize,
    /// Failure rate (0.0-1.0) threshold for opening circuit
    pub failure_rate_threshold: f32,
    /// Sequence of request success/failure outcomes for testing
    pub request_outcomes: Vec<bool>,
}

/// Assertion helpers for common invariant checks.
pub mod assertions {
    use super::{
        AuditEntry, Context, DeliveryAttempt, Duration, HashMap, HashSet, Invariants,
        RequestRecord, Result, Uuid, WebhookEvent, WebhookResponse,
    };

    /// Asserts that all events reach a terminal state.
    pub fn assert_all_terminal(events: &[WebhookEvent]) -> Result<()> {
        Invariants::at_least_once_delivery(events)
            .context("At-least-once delivery invariant violated")
    }

    /// Asserts idempotency for duplicate requests.
    pub fn assert_idempotent(
        original: &WebhookResponse,
        duplicate: &WebhookResponse,
    ) -> Result<()> {
        Invariants::idempotency_guarantee(original, duplicate)
            .context("Idempotency invariant violated")
    }

    /// Asserts retry bounds are respected.
    pub fn assert_retry_bounded(event: &WebhookEvent, max: u32) -> Result<()> {
        Invariants::retry_count_bounded(event, max).context("Retry bound invariant violated")
    }

    /// Asserts proper exponential backoff timing.
    pub fn assert_exponential_backoff(attempts: &[DeliveryAttempt]) -> Result<()> {
        Invariants::exponential_backoff_timing(attempts)
            .context("Exponential backoff invariant violated")
    }

    /// Asserts tenant isolation.
    pub fn assert_tenant_isolated(
        tenant_a: &[WebhookEvent],
        tenant_b: &[WebhookEvent],
        id_a: Uuid,
        id_b: Uuid,
    ) -> Result<()> {
        Invariants::tenant_isolation(tenant_a, tenant_b, id_a, id_b)
            .context("Tenant isolation invariant violated")
    }

    /// Asserts FIFO ordering per endpoint.
    pub fn assert_fifo_ordering(events: &[WebhookEvent]) -> Result<()> {
        Invariants::fifo_ordering_per_endpoint(events).context("FIFO ordering invariant violated")
    }

    /// Asserts no events are lost.
    pub fn assert_no_lost_events<S: ::std::hash::BuildHasher, T: ::std::hash::BuildHasher>(
        ingested: &HashSet<Uuid, S>,
        current: &HashMap<Uuid, WebhookEvent, T>,
    ) -> Result<()> {
        Invariants::no_lost_events(ingested, current).context("No lost events invariant violated")
    }

    /// Asserts complete audit trail.
    pub fn assert_audit_complete(event: &WebhookEvent, entries: &[AuditEntry]) -> Result<()> {
        Invariants::audit_trail_complete(event, entries)
            .context("Audit completeness invariant violated")
    }

    /// Asserts rate limit compliance.
    pub fn assert_rate_limit_respected(
        requests: &[RequestRecord],
        limit: usize,
        window: Duration,
    ) -> Result<()> {
        Invariants::rate_limit_compliance(requests, limit, window)
            .context("Rate limit invariant violated")
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn webhook_events_have_valid_ids(
            event in strategies::webhook_event_strategy()
        ) {
            // All generated events should have non-nil UUIDs
            prop_assert_ne!(event.id, Uuid::nil());
            prop_assert_ne!(event.tenant_id, Uuid::nil());
            prop_assert_ne!(event.endpoint_id, Uuid::nil());
        }

        #[test]
        fn retry_bounds_always_respected(
            event in strategies::webhook_event_strategy(),
            max_retries in 1u32..100
        ) {
            let result = Invariants::retry_count_bounded(&event, max_retries);

            if event.attempt_count <= max_retries + 1 {
                prop_assert!(result.is_ok());
            } else {
                prop_assert!(result.is_err());
            }
        }

        #[test]
        fn idempotency_detection_works(
            id in strategies::uuid_strategy(),
            status in 200u16..204
        ) {
            let response1 = WebhookResponse {
                event_id: id,
                status_code: status,
            };
            let response2 = response1.clone();

            prop_assert!(
                Invariants::idempotency_guarantee(&response1, &response2).is_ok()
            );
        }
    }
}

/// Property-based test strategies for domain types.
pub mod strategies {
    use bytes::Bytes;
    use proptest::{
        collection::{hash_map, vec},
        option,
        prelude::{any, prop_oneof, Just, Strategy},
        string::string_regex,
    };

    use super::{
        CircuitBreakerScenario, Duration, EventStatus, HashMap, RetryScenario, Uuid, WebhookEvent,
    };

    /// Strategy for generating valid webhook events.
    pub fn webhook_event_strategy() -> impl Strategy<Value = WebhookEvent> {
        (
            uuid_strategy(),
            uuid_strategy(),
            uuid_strategy(),
            event_id_strategy(),
            idempotency_strategy(),
            event_status_strategy(),
            0..20u32,
            option::of(timestamp_strategy()),
            headers_strategy(),
            body_strategy(),
            content_type_strategy(),
        )
            .prop_map(
                |(
                    id,
                    tenant_id,
                    endpoint_id,
                    source_event_id,
                    idempotency_strategy,
                    status,
                    attempt_count,
                    next_retry_at,
                    headers,
                    body,
                    content_type,
                )| {
                    WebhookEvent {
                        id,
                        tenant_id,
                        endpoint_id,
                        source_event_id,
                        idempotency_strategy,
                        status,
                        attempt_count,
                        next_retry_at,
                        headers,
                        body,
                        content_type,
                        received_at: {
                            use kapsel_core::Clock;

                            use crate::TestClock;
                            let clock = TestClock::new();
                            chrono::DateTime::<chrono::Utc>::from(clock.now_system())
                        },
                        delivered_at: None,
                    }
                },
            )
    }

    /// Strategy for UUIDs.
    pub fn uuid_strategy() -> impl Strategy<Value = Uuid> {
        any::<[u8; 16]>().prop_map(Uuid::from_bytes)
    }

    /// Strategy for event IDs.
    #[allow(clippy::unwrap_used)]
    pub fn event_id_strategy() -> impl Strategy<Value = String> {
        string_regex("evt_[a-z0-9]{16}").unwrap()
    }

    /// Strategy for idempotency strategies.
    pub fn idempotency_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("header".to_string()),
            Just("content".to_string()),
            Just("source_id".to_string()),
        ]
    }

    /// Strategy for event status.
    pub fn event_status_strategy() -> impl Strategy<Value = EventStatus> {
        prop_oneof![
            Just(EventStatus::Received),
            Just(EventStatus::Pending),
            Just(EventStatus::Delivering),
            Just(EventStatus::Delivered),
            Just(EventStatus::Failed),
        ]
    }

    /// Strategy for HTTP headers.
    #[allow(clippy::unwrap_used)]
    pub fn headers_strategy() -> impl Strategy<Value = HashMap<String, String>> {
        hash_map(
            string_regex("[A-Za-z][A-Za-z0-9-]*").unwrap(),
            string_regex("[^\r\n]*").unwrap(),
            0..10,
        )
    }

    /// Strategy for webhook body.
    #[allow(clippy::unwrap_used)]
    pub fn body_strategy() -> impl Strategy<Value = Bytes> {
        prop_oneof![
            // JSON payload
            string_regex(r#"\{"[a-z]+":"[^"]+"\}"#).unwrap().prop_map(Bytes::from),
            // Form data
            string_regex("[a-z]+=[^&]+(&[a-z]+=[^&]+)*").unwrap().prop_map(Bytes::from),
            // Binary data
            vec(any::<u8>(), 0..1000).prop_map(Bytes::from),
        ]
    }

    /// Strategy for content types.
    pub fn content_type_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("application/json".to_string()),
            Just("application/x-www-form-urlencoded".to_string()),
            Just("text/plain".to_string()),
            Just("application/octet-stream".to_string()),
        ]
    }

    /// Strategy for generating timestamps.
    pub fn timestamp_strategy() -> impl Strategy<Value = chrono::DateTime<chrono::Utc>> {
        (0i64..86400).prop_map(|secs| {
            use kapsel_core::Clock;

            use crate::TestClock;
            let clock = TestClock::new();
            chrono::DateTime::<chrono::Utc>::from(clock.now_system())
                + chrono::Duration::seconds(secs)
        })
    }

    /// Strategy for retry scenarios.
    pub fn retry_scenario_strategy() -> impl Strategy<Value = RetryScenario> {
        (
            1u32..20,                  // attempts
            vec(any::<bool>(), 1..20), // success/failure pattern
            0u64..3_600_000,           // total time ms
        )
            .prop_map(|(max_attempts, outcomes, total_ms)| RetryScenario {
                max_attempts,
                outcomes,
                total_duration: Duration::from_millis(total_ms),
            })
    }

    /// Strategy for circuit breaker scenarios.
    pub fn circuit_breaker_scenario() -> impl Strategy<Value = CircuitBreakerScenario> {
        (
            1usize..10,                 // open threshold
            0.1f32..1.0,                // failure rate threshold
            vec(any::<bool>(), 1..100), // request outcomes
        )
            .prop_map(|(threshold, rate, outcomes)| CircuitBreakerScenario {
                open_threshold: threshold,
                failure_rate_threshold: rate,
                request_outcomes: outcomes,
            })
    }
}
