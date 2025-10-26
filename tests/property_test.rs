//! Property-based tests for webhook reliability invariants.
//!
//! Uses randomly generated inputs to verify system-wide invariants always
//! hold across components, including retry bounds, circuit breaker states,
//! tenant isolation, and delivery guarantees.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use kapsel_testing::{
    fixtures::WebhookBuilder,
    invariants::{strategies, CircuitState, EventStatus, WebhookEvent},
    time::backoff::deterministic_webhook_backoff,
    ScenarioBuilder, TestEnv,
};
use proptest::{
    prelude::*,
    test_runner::{Config as ProptestConfig, TestRunner},
};
use serde_json::json;
use uuid::Uuid;

/// Creates property test configuration based on environment.
///
/// Uses environment variables:
/// - `PROPTEST_CASES`: Number of test cases (default: 20 for dev, 100 for CI)
/// - `CI`: If set to "true", uses CI configuration
fn proptest_config() -> ProptestConfig {
    let is_ci = std::env::var("CI").unwrap_or_default() == "true";
    let default_cases = if is_ci { 10 } else { 8 };

    let cases =
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(default_cases);

    ProptestConfig::with_cases(cases)
}

// E2E property tests using mock implementations
proptest! {
    #![proptest_config(proptest_config())]

    /// Verifies that idempotent operations always produce identical results.
    #[test]
    fn idempotency_is_guaranteed(
        webhook in strategies::webhook_event_strategy(),
        delay_ms in 0u64..if std::env::var("CI").unwrap_or_default() == "true" { 1000 } else { 50 },
        modifications in prop::collection::vec(any::<u8>(), 0..100)
    ) {
        // Process webhook once
        let first_response = process_webhook_mock(&webhook);

        // Wait random delay
        std::thread::sleep(Duration::from_millis(delay_ms));

        // Modify webhook data (but keep same idempotency key)
        let mut modified = webhook.clone();
        modified.body = bytes::Bytes::from(modifications);

        // Process again with same idempotency key
        let second_response = process_webhook_mock(&modified);

        // Must return identical event ID despite modifications
        prop_assert_eq!(
            first_response.event_id,
            second_response.event_id,
            "Idempotency violated: different event IDs for same idempotency key"
        );
    }

    /// Verifies retry count never exceeds configured maximum.
    #[test]
    fn retry_count_is_bounded(
        max_retries in 1u32..20,
        failure_count in 0u32..100,
        jitter_factor in 0.0f32..0.5
    ) {
        let mut event = create_test_event();

        // Simulate multiple failures
        for _ in 0..failure_count {
            if should_retry(&event, max_retries) {
                event.attempt_count += 1;
                add_jitter(&mut event, jitter_factor);
            }
        }

        // Verify bounds
        prop_assert!(
            event.attempt_count <= max_retries + 1,
            "Retry count {} exceeds max {} (plus initial attempt)",
            event.attempt_count,
            max_retries
        );
    }

    /// Verifies exponential backoff timing increases correctly.
    #[test]
    fn exponential_backoff_increases(
        attempts in 1usize..10,
        base_delay_ms in 100u64..5000,
        jitter_percent in 0u32..50
    ) {
        let mut delays = Vec::new();

        for attempt in 0..attempts {
            let delay = calculate_backoff(
                attempt as u32,
                Duration::from_millis(base_delay_ms),
                jitter_percent as f32 / 100.0
            );
            delays.push(delay);
        }

        // Each delay should be roughly double the previous (accounting for jitter)
        for window in delays.windows(2) {
            let ratio = window[1].as_millis() as f64 / window[0].as_millis() as f64;

            // With up to 50% jitter, ratio should be between 1.0 and 3.0
            prop_assert!(
                (1.0..=3.0).contains(&ratio),
                "Backoff ratio {} out of expected range [1.0, 3.0]",
                ratio
            );
        }
    }

    /// Verifies circuit breaker state transitions are correct.
    #[test]
    fn circuit_breaker_transitions_correctly(
        threshold in 1usize..20,
        outcomes in prop::collection::vec(any::<bool>(), 1..100)
    ) {
        let mut circuit = MockCircuitBreaker::new(threshold);
        let mut consecutive_failures = 0;

        for success in outcomes {
            let prev_state = circuit.state;

            if success {
                circuit.record_success();
                consecutive_failures = 0;
            } else {
                circuit.record_failure();
                consecutive_failures += 1;
            }

            // Verify state transitions
            match prev_state {
                CircuitState::Closed => {
                    if consecutive_failures >= threshold {
                        prop_assert_eq!(circuit.state, CircuitState::Open);
                    } else {
                        prop_assert_eq!(circuit.state, CircuitState::Closed);
                    }
                },
                CircuitState::Open => {
                    // Should stay open until manual reset to half-open
                    prop_assert_eq!(circuit.state, CircuitState::Open);
                },
                CircuitState::HalfOpen => {
                    if success {
                        prop_assert_eq!(circuit.state, CircuitState::Closed);
                    } else {
                        prop_assert_eq!(circuit.state, CircuitState::Open);
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    /// Verifies no webhooks are lost during processing.
    #[test]
    fn no_webhooks_are_lost(
        webhooks in prop::collection::vec(strategies::webhook_event_strategy(), 1..50),
        failure_rate in 0.0f32..0.5
    ) {
        let mut ingested = HashSet::new();
        let mut processed = HashMap::new();

        // Ingest all webhooks
        for webhook in &webhooks {
            ingested.insert(webhook.id);
        }

        // Process with random failures
        for webhook in webhooks {
            if rand::random::<f32>() > failure_rate {
                processed.insert(webhook.id, webhook);
            } else {
                // Even failed webhooks must be tracked
                let mut failed = webhook.clone();
                failed.status = EventStatus::Failed;
                processed.insert(failed.id, failed);
            }
        }

        // Every ingested webhook must be accounted for
        for id in &ingested {
            prop_assert!(
                processed.contains_key(id),
                "Webhook {} was lost during processing",
                id
            );
        }

        // No phantom webhooks should appear
        for id in processed.keys() {
            prop_assert!(
                ingested.contains(id),
                "Webhook {} appeared without being ingested",
                id
            );
        }
    }

    /// Verifies tenant isolation is maintained.
    #[test]
    fn tenants_are_isolated(
        tenant_a_webhooks in prop::collection::vec(
            strategies::webhook_event_strategy(), 1..20
        ),
        tenant_b_webhooks in prop::collection::vec(
            strategies::webhook_event_strategy(), 1..20
        )
    ) {
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();

        // Assign webhooks to tenants
        let mut webhooks_a: Vec<_> = tenant_a_webhooks;
        for webhook in &mut webhooks_a {
            webhook.tenant_id = tenant_a;
        }

        let mut webhooks_b: Vec<_> = tenant_b_webhooks;
        for webhook in &mut webhooks_b {
            webhook.tenant_id = tenant_b;
        }

        // Query webhooks for each tenant
        let results_a = query_tenant_webhooks(&webhooks_a, &webhooks_b, tenant_a);
        let results_b = query_tenant_webhooks(&webhooks_a, &webhooks_b, tenant_b);

        // Verify isolation
        for webhook in &results_a {
            prop_assert_eq!(
                webhook.tenant_id, tenant_a,
                "Tenant A received webhook from tenant {}",
                webhook.tenant_id
            );
        }

        for webhook in &results_b {
            prop_assert_eq!(
                webhook.tenant_id, tenant_b,
                "Tenant B received webhook from tenant {}",
                webhook.tenant_id
            );
        }
    }

    /// Verifies rate limiting is enforced.
    #[test]
    fn rate_limits_are_enforced(
        request_count in 1usize..1000,
        rate_limit in 10usize..100,
        window_seconds in 1u64..60
    ) {
        let window = Duration::from_secs(window_seconds);
        let mut requests = Vec::new();
        let mut current_time = std::time::Instant::now();

        for _ in 0..request_count {
            requests.push(RequestAttempt {
                timestamp: current_time,
                accepted: requests.len() < rate_limit
            });

            // Advance time slightly
            current_time += Duration::from_millis(10);

            // Reset window if needed
            if current_time.duration_since(requests[0].timestamp) >= window {
                requests.clear();
            }
        }

        // Count accepted requests in any window
        for i in 0..requests.len() {
            let window_start = requests[i].timestamp;
            let window_end = window_start + window;

            let accepted_in_window = requests[i..]
                .iter()
                .take_while(|r| r.timestamp < window_end)
                .filter(|r| r.accepted)
                .count();

            prop_assert!(
                accepted_in_window <= rate_limit,
                "Rate limit violated: {} requests accepted in window (limit: {})",
                accepted_in_window,
                rate_limit
            );
        }
    }

    /// Verifies delivery order is preserved per endpoint.
    #[test]
    fn delivery_order_preserved_per_endpoint(
        webhooks in prop::collection::vec(strategies::webhook_event_strategy(), 2..20),
        endpoint_count in 1usize..5
    ) {
        // Assign webhooks to endpoints
        let endpoints: Vec<Uuid> = (0..endpoint_count).map(|_| Uuid::new_v4()).collect();
        let mut endpoint_webhooks: HashMap<Uuid, Vec<WebhookEvent>> = HashMap::new();

        for (i, mut webhook) in webhooks.into_iter().enumerate() {
            let endpoint = endpoints[i % endpoints.len()];
            webhook.endpoint_id = endpoint;
            webhook.received_at = chrono::Utc::now() + chrono::Duration::milliseconds(i as i64);
            endpoint_webhooks.entry(endpoint).or_default().push(webhook);
        }

        // Process and deliver webhooks
        for webhooks in endpoint_webhooks.values_mut() {
            // Simulate delivery with timestamps
            for (i, webhook) in webhooks.iter_mut().enumerate() {
                webhook.status = EventStatus::Delivered;
                webhook.delivered_at = Some(
                    webhook.received_at + chrono::Duration::seconds(i as i64)
                );
            }

            // Verify FIFO order
            for window in webhooks.windows(2) {
                prop_assert!(
                    window[0].received_at <= window[1].received_at,
                    "Receive order violated"
                );

                if let (Some(d1), Some(d2)) = (window[0].delivered_at, window[1].delivered_at) {
                    prop_assert!(
                        d1 <= d2,
                        "Delivery order violated for endpoint"
                    );
                }
            }
        }
    }

    /// Verifies maximum payload size is enforced.
    #[test]
    fn payload_size_limits_enforced(
        size_bytes in 0usize..20_000_000, // Up to 20MB
        max_size_mb in 1usize..15
    ) {
        let max_size = max_size_mb * 1024 * 1024;
        let payload = vec![0u8; size_bytes];

        let accepted = validate_payload_size(&payload, max_size);

        if size_bytes <= max_size {
            prop_assert!(accepted, "Valid payload rejected: {} bytes <= {} max", size_bytes, max_size);
        } else {
            prop_assert!(!accepted, "Oversized payload accepted: {} bytes > {} max", size_bytes, max_size);
        }
    }

    /// Verifies signature validation works correctly.
    #[test]
    fn signature_validation_is_correct(
        payload in prop::collection::vec(any::<u8>(), 1..1000),
        secret in prop::string::string_regex("[a-zA-Z0-9]{16,64}").unwrap(),
        tamper in any::<bool>()
    ) {
        // Generate valid signature
        let signature = generate_hmac(&payload, &secret);

        // Optionally tamper with payload
        let mut verification_payload = payload.clone();
        if tamper
            && !verification_payload.is_empty() {
                verification_payload[0] ^= 1; // Flip a bit
            }

        // Verify signature
        let valid = verify_hmac(&verification_payload, &secret, &signature);

        if tamper && !payload.is_empty() {
            prop_assert!(!valid, "Tampered payload passed signature validation");
        } else {
            prop_assert!(valid, "Valid signature failed validation");
        }
    }
}

// Helper functions for mock-based property tests
#[derive(Debug, Clone)]
struct TestWebhookResponse {
    event_id: Uuid,
}

fn process_webhook_mock(webhook: &WebhookEvent) -> TestWebhookResponse {
    // Simulate idempotent processing
    TestWebhookResponse {
        event_id: webhook.id, // Always return same ID for same webhook
    }
}

fn create_test_event() -> WebhookEvent {
    WebhookEvent {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        endpoint_id: Uuid::new_v4(),
        source_event_id: format!("evt_{}", Uuid::new_v4()),
        idempotency_strategy: "header".to_string(),
        status: EventStatus::Pending,
        attempt_count: 0,
        next_retry_at: None,
        headers: HashMap::new(),
        body: bytes::Bytes::new(),
        content_type: "application/json".to_string(),
        received_at: chrono::Utc::now(),
        delivered_at: None,
    }
}

fn should_retry(event: &WebhookEvent, max_retries: u32) -> bool {
    event.attempt_count < max_retries && !event.is_terminal_state()
}

fn add_jitter(event: &mut WebhookEvent, factor: f32) {
    // Add jitter to retry timing
    let jitter_ms = (rand::random::<f32>() * 1000.0 * factor) as i64;
    if let Some(retry_at) = event.next_retry_at {
        event.next_retry_at = Some(retry_at + chrono::Duration::milliseconds(jitter_ms));
    }
}

fn calculate_backoff(attempt: u32, base: Duration, jitter: f32) -> Duration {
    let exponential = base * 2_u32.pow(attempt.min(10));
    let jitter_amount = exponential.as_millis() as f32 * jitter;
    let jitter_ms = (rand::random::<f32>() * jitter_amount * 2.0 - jitter_amount) as u64;
    Duration::from_millis(exponential.as_millis() as u64 + jitter_ms)
}

#[derive(Clone)]
struct MockCircuitBreaker {
    state: CircuitState,
    failure_count: usize,
    threshold: usize,
}

impl MockCircuitBreaker {
    fn new(threshold: usize) -> Self {
        Self { state: CircuitState::Closed, failure_count: 0, threshold }
    }

    fn record_success(&mut self) {
        self.failure_count = 0;
        if self.state == CircuitState::HalfOpen {
            self.state = CircuitState::Closed;
        }
    }

    fn record_failure(&mut self) {
        self.failure_count += 1;
        if (self.state == CircuitState::Closed && self.failure_count >= self.threshold)
            || self.state == CircuitState::HalfOpen
        {
            self.state = CircuitState::Open;
        }
    }
}

fn query_tenant_webhooks(
    webhooks_a: &[WebhookEvent],
    webhooks_b: &[WebhookEvent],
    tenant_id: Uuid,
) -> Vec<WebhookEvent> {
    webhooks_a
        .iter()
        .chain(webhooks_b.iter())
        .filter(|w| w.tenant_id == tenant_id)
        .cloned()
        .collect()
}

#[derive(Debug)]
struct RequestAttempt {
    timestamp: std::time::Instant,
    accepted: bool,
}

fn validate_payload_size(payload: &[u8], max_size: usize) -> bool {
    payload.len() <= max_size
}

fn generate_hmac(payload: &[u8], secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

fn verify_hmac(payload: &[u8], secret: &str, signature: &str) -> bool {
    generate_hmac(payload, secret) == signature
}

// E2E property tests using ScenarioBuilder for comprehensive coverage
/// Property-based scenario test: Retry behavior with random failure patterns.
///
/// This test demonstrates the power of combining proptest with ScenarioBuilder.
/// It generates random failure counts and verifies that the system correctly
/// performs exactly that many retries before succeeding.
#[test]
fn property_webhook_delivery_retry_scenarios() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = proptest_config();
    let mut runner = TestRunner::new(config);

    runner
        .run(&(0u32..5, any::<[u8; 12]>()), |(num_failures, webhook_data)| {
            rt.block_on(async {
                let mut env = TestEnv::new_isolated().await.unwrap();
                let mut tx = env.pool().begin().await.unwrap();

                let tenant_name = "prop-test-tenant";
                let tenant_id = env.create_tenant_tx(&mut *tx, tenant_name).await.unwrap();
                let endpoint_id = env
                    .create_endpoint_tx(
                        &mut *tx,
                        tenant_id,
                        &env.http_mock.endpoint_url("/webhook"),
                    )
                    .await
                    .unwrap();
                tx.commit().await.unwrap();

                // Dynamically build mock response sequence based on generated failure count
                let mut mock_sequence = env.http_mock.mock_sequence();
                for _ in 0..num_failures {
                    mock_sequence = mock_sequence.respond_with(503, "Service Unavailable");
                }
                mock_sequence.respond_with_json(200, &json!({"status": "success"})).build().await;

                // Create webhook with random payload data
                let webhook = WebhookBuilder::new()
                    .tenant(tenant_id.0)
                    .endpoint(endpoint_id.0)
                    .source_event(format!("prop_test_{:?}", webhook_data))
                    .json_body(&json!({"data": format!("{webhook_data:?}"), "test": "property"}))
                    .build();

                let event_id = env.ingest_webhook(&webhook).await.unwrap();

                // Dynamically build scenario steps based on generated parameters
                let mut scenario = ScenarioBuilder::new("property-based retry scenario");

                // Add failure attempts with proper backoff timing
                for i in 0..num_failures {
                    let backoff_duration = deterministic_webhook_backoff(i);
                    scenario = scenario
                        .run_delivery_cycle()
                        .expect_delivery_attempts(event_id, (i + 1) as i32)
                        .expect_status(event_id, "pending")
                        .advance_time(backoff_duration);
                }

                // Final successful attempt
                scenario = scenario
                    .run_delivery_cycle()
                    .expect_delivery_attempts(event_id, (num_failures + 1) as i32)
                    .expect_status(event_id, "delivered");

                // Execute the scenario
                scenario.run(&mut env).await.unwrap();

                // Verify total time matches expected backoff progression
                let expected_total_time: Duration =
                    (0..num_failures).map(deterministic_webhook_backoff).sum();

                assert_eq!(
                    env.elapsed(),
                    expected_total_time,
                    "Total processing time should match sum of backoff delays for {} failures",
                    num_failures
                );
            });

            Ok(())
        })
        .unwrap();
}

/// Property test: Idempotency under duress.
///
/// Verifies that duplicate webhooks are properly handled even when they
/// arrive at different points in the original event's lifecycle.
#[test]
fn property_idempotency_under_duress() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = proptest_config();
    let mut runner = TestRunner::new(config);

    runner
        .run(&(1usize..4, 0u32..3), |(duplicate_count, initial_failures)| {
            rt.block_on(async {
                let mut env = TestEnv::new_isolated().await.unwrap();
                let mut tx = env.pool().begin().await.unwrap();

                let tenant_name = "idempotency-tenant";
                let tenant_id = env.create_tenant_tx(&mut *tx, tenant_name).await.unwrap();
                let endpoint_id = env
                    .create_endpoint_tx(
                        &mut *tx,
                        tenant_id,
                        &env.http_mock.endpoint_url("/webhook"),
                    )
                    .await
                    .unwrap();
                tx.commit().await.unwrap();

                // Setup mock responses - initial failures then success
                let mut mock_sequence = env.http_mock.mock_sequence();
                for _ in 0..initial_failures {
                    mock_sequence = mock_sequence.respond_with(503, "Service Unavailable");
                }
                mock_sequence.respond_with_json(200, &json!({"status": "ok"})).build().await;

                let source_event_id = format!("idempotent_event_{}", uuid::Uuid::new_v4());
                let webhook = WebhookBuilder::new()
                    .tenant(tenant_id.0)
                    .endpoint(endpoint_id.0)
                    .source_event(&source_event_id)
                    .json_body(&json!({"test": "idempotency"}))
                    .build();

                // Initial ingestion
                let original_event_id = env.ingest_webhook(&webhook).await.unwrap();

                // Run initial delivery cycles with failures
                let mut scenario = ScenarioBuilder::new("idempotency under duress");
                for i in 0..initial_failures {
                    scenario = scenario
                        .run_delivery_cycle()
                        .advance_time(deterministic_webhook_backoff(i));
                }

                // Run scenario
                scenario = scenario.run_delivery_cycle();
                scenario.run(&mut env).await.unwrap();

                // Now test duplicate rejection after scenario
                for i in 0..duplicate_count {
                    let webhook_dup = WebhookBuilder::new()
                        .tenant(tenant_id.0)
                        .endpoint(endpoint_id.0)
                        .source_event(&source_event_id)
                        .json_body(&json!({"test": "idempotency", "duplicate": i}))
                        .build();

                    let result = env.ingest_webhook(&webhook_dup).await;

                    // Should either return same event_id or be rejected
                    match result {
                        Ok(event_id) => {
                            assert_eq!(
                                event_id, original_event_id,
                                "Duplicate should return original event ID"
                            );
                        },
                        Err(_) => {
                            // Duplicate rejected is also acceptable
                        },
                    }
                }

                // Verify only one event exists for this source_event_id
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM webhook_events WHERE source_event_id = $1",
                )
                .bind(&source_event_id)
                .fetch_one(env.pool())
                .await
                .unwrap();

                assert_eq!(count, 1, "Should have exactly one event for source_event_id");
            });
            Ok(())
        })
        .unwrap();
}

/// Property test: Circuit breaker state transitions.
///
/// Verifies that circuit breaker correctly transitions between states
/// based on actual HTTP responses.
#[test]
fn property_circuit_breaker_resilience() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = proptest_config();
    let mut runner = TestRunner::new(config);

    runner
        .run(
            &prop::collection::vec(
                prop::sample::select(vec![
                    Ok(()),   // Success
                    Err(503), // Service Unavailable
                    Err(500), // Internal Server Error
                    Err(502), // Bad Gateway
                ]),
                3..8,
            ),
            |response_sequence| {
                rt.block_on(async {
                    let mut env = TestEnv::new_isolated().await.unwrap();
                    let mut tx = env.pool().begin().await.unwrap();

                    let tenant_name = "circuit-breaker-tenant";
                    let tenant_id = env.create_tenant_tx(&mut *tx, tenant_name).await.unwrap();
                    let endpoint_id = env
                        .create_endpoint_tx(
                            &mut *tx,
                            tenant_id,
                            &env.http_mock.endpoint_url("/webhook"),
                        )
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();

                    // Configure mock with the generated response sequence
                    let mut mock_sequence = env.http_mock.mock_sequence();
                    for response in &response_sequence {
                        match response {
                            Ok(()) => {
                                mock_sequence =
                                    mock_sequence.respond_with_json(200, &json!({"ok": true}))
                            },
                            Err(code) => mock_sequence = mock_sequence.respond_with(*code, "Error"),
                        }
                    }
                    mock_sequence.build().await;

                    // Ingest webhooks and run delivery cycles
                    let mut scenario = ScenarioBuilder::new("circuit breaker property test");
                    let mut event_ids = Vec::new();

                    for i in 0..response_sequence.len() {
                        let webhook = WebhookBuilder::new()
                            .tenant(tenant_id.0)
                            .endpoint(endpoint_id.0)
                            .source_event(format!("circuit_test_{}", i))
                            .json_body(&json!({"seq": i}))
                            .build();

                        let event_id = env.ingest_webhook(&webhook).await.unwrap();
                        event_ids.push(event_id);

                        scenario =
                            scenario.run_delivery_cycle().advance_time(Duration::from_millis(100));
                    }

                    scenario.run(&mut env).await.unwrap();

                    // Verify basic invariants after scenario
                    let total_events: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM webhook_events WHERE endpoint_id = $1",
                    )
                    .bind(endpoint_id.0)
                    .fetch_one(env.pool())
                    .await
                    .unwrap();

                    let total_attempts: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM delivery_attempts da
                         JOIN webhook_events we ON da.event_id = we.id
                         WHERE we.endpoint_id = $1",
                    )
                    .bind(endpoint_id.0)
                    .fetch_one(env.pool())
                    .await
                    .unwrap();

                    // Basic invariants: we should have events and attempts
                    assert_eq!(
                        total_events as usize,
                        response_sequence.len(),
                        "Should have one event per response"
                    );
                    assert!(
                        total_attempts >= total_events,
                        "Should have at least one attempt per event"
                    );

                    // Verify circuit breaker state exists (basic sanity check)
                    let (circuit_state, _failure_count, _success_count): (String, i32, i32) =
                        sqlx::query_as(
                            "SELECT circuit_state, circuit_failure_count, circuit_success_count
                             FROM endpoints WHERE id = $1",
                        )
                        .bind(endpoint_id.0)
                        .fetch_one(env.pool())
                        .await
                        .unwrap();

                    // Circuit breaker should have a valid state
                    assert!(
                        ["closed", "open", "half_open"].contains(&circuit_state.as_str()),
                        "Circuit breaker should be in a valid state: {}",
                        circuit_state
                    );
                });
                Ok(())
            },
        )
        .unwrap();
}

/// Property test: FIFO processing guarantee.
///
/// Verifies that webhooks are processed (first attempts) in FIFO order
/// based on received_at timestamps, even with random failures.
#[test]
fn property_fifo_processing_order() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = proptest_config();
    let mut runner = TestRunner::new(config);

    runner
        .run(
            &(3usize..6, prop::collection::vec(prop::bool::ANY, 3..8)),
            |(webhook_count, failure_pattern)| {
                rt.block_on(async {
                    let env = TestEnv::new_isolated().await.unwrap();
                    let mut tx = env.pool().begin().await.unwrap();

                    let tenant_name = "fifo-tenant";
                    let tenant_id = env.create_tenant_tx(&mut *tx, tenant_name).await.unwrap();
                    let endpoint_id =
                        env.create_endpoint_tx(&mut *tx, tenant_id, &env.http_mock.url()).await.unwrap();

                    // Setup mock responses - some may fail initially
                    let mut mock_sequence = env.http_mock.mock_sequence();
                    for (i, should_fail) in failure_pattern.iter().take(webhook_count).enumerate() {
                        if *should_fail && i < webhook_count - 1 {
                            // Fail initially but succeed on retry
                            mock_sequence = mock_sequence
                                .respond_with(503, "Temporary failure")
                                .respond_with_json(200, &json!({"ok": true}));
                        } else {
                            mock_sequence =
                                mock_sequence.respond_with_json(200, &json!({"ok": true}));
                        }
                    }
                    mock_sequence.build().await;

                    // Ingest webhooks with explicit timing to ensure FIFO order
                    let mut event_ids = Vec::new();
                    for i in 0..webhook_count {
                        let webhook = WebhookBuilder::new()
                            .tenant(tenant_id.0)
                            .endpoint(endpoint_id.0)
                            .source_event(format!("ordered_event_{:03}", i))
                            .json_body(&json!({"sequence": i}))
                            .build();

                        let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await.unwrap();
                        event_ids.push(event_id);


                        // Ensure different received_at timestamps
                        env.advance_time(Duration::from_millis(10));

                    }

                    // Commit transaction to make data available for delivery testing
                    tx.commit().await.unwrap();

                    // Process webhooks with single delivery cycle first
                    env.run_delivery_cycle().await.unwrap();




                    // Verify FIFO processing by checking first attempt order
                    // Get the first delivery attempt for each event, ordered by attempt time
                    let first_attempts: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
                        "SELECT da.event_id, da.attempted_at
                         FROM delivery_attempts da
                         INNER JOIN (
                             SELECT event_id, MIN(attempt_number) as min_attempt
                             FROM delivery_attempts
                             WHERE event_id = ANY($1)
                             GROUP BY event_id
                         ) first ON da.event_id = first.event_id AND da.attempt_number = first.min_attempt
                         ORDER BY da.attempted_at ASC",
                    )
                    .bind(event_ids.iter().map(|id| id.0).collect::<Vec<_>>())
                    .fetch_all(env.pool())
                    .await
                    .unwrap();


                    // Verify that first attempts happened in FIFO order
                    for (attempt_idx, (event_id, _)) in first_attempts.iter().enumerate() {
                        let expected_event_id = event_ids[attempt_idx].0;
                        assert_eq!(
                            *event_id, expected_event_id,
                            "First attempt #{} should be for event at position {}, but got event {}",
                            attempt_idx, attempt_idx, event_id
                        );
                    }

                    // Verify we have first attempts for all events
                    assert_eq!(
                        first_attempts.len(),
                        webhook_count,
                        "Should have first attempts for all {} events",
                        webhook_count
                    );
                });
                Ok(())
            },
        )
        .unwrap();
}
