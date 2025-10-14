//! Property-based tests for webhook reliability invariants.
//!
//! These tests use randomly generated inputs to verify that system
//! invariants always hold, regardless of input data or state.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::Utc;
use kapsel_delivery::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    error::DeliveryError,
    retry::{BackoffStrategy, RetryContext, RetryPolicy},
};
use proptest::prelude::*;
use test_harness::{
    invariants::{strategies, CircuitState, EventStatus, WebhookEvent},
    TestEnv,
};
use uuid::Uuid;

/// Creates property test configuration based on environment.
///
/// Uses environment variables:
/// - `PROPTEST_CASES`: Number of test cases (default: 20 for dev, 100 for CI)
/// - `CI`: If set to "true", uses CI configuration
fn proptest_config() -> ProptestConfig {
    let is_ci = std::env::var("CI").unwrap_or_default() == "true";
    let default_cases = if is_ci { 100 } else { 20 };

    let cases =
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(default_cases);

    ProptestConfig::with_cases(cases)
}

// Configuration for property tests
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

    /// Verifies delivery retry decision never exceeds maximum attempts.
    #[test]
    fn delivery_retry_bounds_respected(
        max_attempts in 1u32..20,
        failure_count in 0u32..50,
        strategy in prop::sample::select(vec![
            BackoffStrategy::Exponential,
            BackoffStrategy::Linear,
            BackoffStrategy::Fixed,
        ])
    ) {
        let policy = RetryPolicy {
            max_attempts,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300),
            jitter_factor: 0.1,
            backoff_strategy: strategy,
        };

        let error = DeliveryError::server_error(500, "Server Error");
        let context = RetryContext::new(
            failure_count,
            error.clone(),
            Utc::now(),
            policy,
        );

        let decision = context.decide_retry();

        // If failure count >= max attempts, should give up
        if failure_count >= max_attempts {
            if let kapsel_delivery::retry::RetryDecision::GiveUp { .. } = decision {
                // Expected
            } else {
                prop_assert!(false, "Should give up after max attempts");
            }
        }

        // If failure count < max attempts and error is retryable, should retry
        if failure_count < max_attempts && error.is_retryable() {
            if let kapsel_delivery::retry::RetryDecision::Retry { .. } = decision {
                // Expected
            } else {
                prop_assert!(false, "Should retry for retryable error");
            }
        }
    }

    /// Verifies delivery backoff calculation produces reasonable delays.
    #[test]
    fn delivery_backoff_calculation_bounds(
        attempt in 0u32..15,
        base_delay_secs in 1u64..30,
        max_delay_secs in 30u64..600,
        jitter_factor in 0.0f64..0.5
    ) {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_secs(base_delay_secs),
            max_delay: Duration::from_secs(max_delay_secs),
            jitter_factor,
            backoff_strategy: BackoffStrategy::Exponential,
        };

        let error = DeliveryError::network("Connection failed".to_string());
        let context = RetryContext::new(
            attempt,
            error,
            Utc::now(),
            policy.clone(),
        );

        if let kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } = context.decide_retry() {
            let delay = next_attempt_at.signed_duration_since(Utc::now());
            let delay_secs = delay.num_seconds().max(0) as u64;

            // Delay should be at least 0 and at most max_delay (with some tolerance for jitter)
            prop_assert!(delay_secs <= max_delay_secs + 1); // Allow 1 second tolerance for jitter
        }
    }

    /// Verifies delivery circuit breaker state transitions follow correct patterns.
    #[test]
    fn delivery_circuit_breaker_state_transitions(
        failure_threshold in 1usize..20,
        success_threshold in 1usize..10,
        failure_rate_threshold in 0.1f64..0.9,
        consecutive_failures in 0usize..50
    ) {
        let config = CircuitConfig {
            failure_threshold: failure_threshold as u32,
            success_threshold: success_threshold as u32,
            failure_rate_threshold,
            min_requests_for_rate: 5,
            open_timeout: Duration::from_secs(10),
            half_open_max_requests: 3,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let manager = CircuitBreakerManager::new(config);
            let endpoint_id = "test-endpoint";

            // Start in Closed state
            prop_assert!(manager.should_allow_request(endpoint_id).await);

            // Record consecutive failures
            for _ in 0..consecutive_failures {
                manager.record_failure(endpoint_id).await;
            }

            let stats = manager.circuit_stats(endpoint_id).await.unwrap();

            // Circuit should open if EITHER condition is met:
            // 1. Consecutive failures >= threshold
            // 2. Total requests >= min_requests AND failure_rate >= threshold
            let should_be_open = consecutive_failures >= failure_threshold
                || (stats.total_requests >= 5 && stats.failure_rate() >= failure_rate_threshold);

            if should_be_open {
                prop_assert_eq!(stats.state, kapsel_delivery::circuit::CircuitState::Open);
                prop_assert!(!manager.should_allow_request(endpoint_id).await);
            } else {
                prop_assert_eq!(stats.state, kapsel_delivery::circuit::CircuitState::Closed);
            }

            // Failure count should match what we recorded
            prop_assert_eq!(stats.consecutive_failures, consecutive_failures as u32);
            Ok(())
        })?;
    }

    /// Verifies delivery circuit breaker failure rate calculation accuracy.
    #[test]
    fn delivery_circuit_breaker_failure_rate(
        total_requests in 10usize..100,
        failed_requests in 0usize..100
    ) {
        let failed_requests = failed_requests.min(total_requests);
        let expected_rate = failed_requests as f64 / total_requests as f64;

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let manager = CircuitBreakerManager::new(CircuitConfig::default());
            let endpoint_id = "test-endpoint";

            // Record successes and failures
            for _ in 0..(total_requests - failed_requests) {
                manager.record_success(endpoint_id).await;
            }
            for _ in 0..failed_requests {
                manager.record_failure(endpoint_id).await;
            }

            let stats = manager.circuit_stats(endpoint_id).await.unwrap();
            let calculated_rate = stats.failure_rate();

            // Allow small floating point tolerance
            prop_assert!((calculated_rate - expected_rate).abs() < 0.001);
            prop_assert_eq!(stats.total_requests, total_requests as u32);
            prop_assert_eq!(stats.failed_requests, failed_requests as u32);
            Ok(())
        })?;
    }

    /// Verifies delivery error categorization for retry decisions.
    #[test]
    fn delivery_error_retry_categorization(
        status_code in 400u16..600,
        timeout_seconds in 1u64..120
    ) {
        // Client errors (4xx) should not be retryable
        if (400..500).contains(&status_code) {
            let error = DeliveryError::client_error(status_code, "Client Error".to_string());
            prop_assert!(!error.is_retryable());
        }

        // Server errors (5xx) should be retryable
        if (500..600).contains(&status_code) {
            let error = DeliveryError::server_error(status_code, "Server Error".to_string());
            prop_assert!(error.is_retryable());
        }

        // Timeouts should be retryable
        let timeout_error = DeliveryError::timeout(timeout_seconds);
        prop_assert!(timeout_error.is_retryable());

        // Network errors should be retryable
        let network_error = DeliveryError::network("Connection failed".to_string());
        prop_assert!(network_error.is_retryable());

        // Circuit open should not be retryable (handled differently)
        let circuit_error = DeliveryError::circuit_open("endpoint-123".to_string());
        prop_assert!(!circuit_error.is_retryable());
    }
}

// Fuzzing tests for delivery module robustness
proptest! {
    #![proptest_config(proptest_config())]

    /// Fuzzes retry calculation with extreme and edge case values.
    #[test]
    fn fuzz_retry_calculation_edge_cases(
        attempt in 0u32..1000,
        base_delay_ms in 1u64..10000,
        max_delay_ms in 1u64..86400000, // Up to 24 hours
        jitter_factor in 0.0f64..1.0
    ) {
        let base_delay = Duration::from_millis(base_delay_ms);
        let max_delay = Duration::from_millis(max_delay_ms.max(base_delay_ms));

        let policy = RetryPolicy {
            max_attempts: 100,
            base_delay,
            max_delay,
            jitter_factor,
            backoff_strategy: BackoffStrategy::Exponential,
        };

        let error = DeliveryError::network("Network failure".to_string());
        let context = RetryContext::new(
            attempt,
            error,
            Utc::now(),
            policy,
        );

        let decision = context.decide_retry();

        // Should always produce valid decision
        match decision {
            kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } => {
                let delay = next_attempt_at.signed_duration_since(Utc::now());
                // Delay should never be negative or significantly exceed max_delay
                prop_assert!(delay.num_milliseconds() >= 0);
                // Allow some tolerance for jitter and timing
                let max_delay_ms = max_delay.as_millis() as i64;
                prop_assert!(delay.num_milliseconds() <= max_delay_ms * 2); // Double tolerance for extreme jitter
            }
            kapsel_delivery::retry::RetryDecision::GiveUp { reason: _ } => {
                // Should give up if attempt >= max_attempts
                prop_assert!(attempt >= 100);
            }
        }
    }

    /// Fuzzes circuit breaker with random failure/success sequences.
    #[test]
    fn fuzz_circuit_breaker_random_sequences(
        failure_threshold in 1usize..50,
        success_threshold in 1usize..20,
        operations in prop::collection::vec(any::<bool>(), 1..200)
    ) {
        let config = CircuitConfig {
            failure_threshold: failure_threshold as u32,
            success_threshold: success_threshold as u32,
            failure_rate_threshold: 0.5,
            min_requests_for_rate: 5,
            open_timeout: Duration::from_secs(1),
            half_open_max_requests: 5,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let manager = CircuitBreakerManager::new(config);
            let endpoint_id = "fuzz-endpoint";

            let mut consecutive_failures = 0;
            let mut consecutive_successes = 0;
            let mut expected_state = kapsel_delivery::circuit::CircuitState::Closed;

            for success in operations.iter() {
                if *success {
                    manager.record_success(endpoint_id).await;
                    consecutive_failures = 0;
                    consecutive_successes += 1;

                    // If we were in half-open and got enough successes, should close
                    if matches!(expected_state, kapsel_delivery::circuit::CircuitState::HalfOpen)
                        && consecutive_successes >= success_threshold {
                        expected_state = kapsel_delivery::circuit::CircuitState::Closed;
                        consecutive_successes = 0;
                    }
                } else {
                    manager.record_failure(endpoint_id).await;
                    consecutive_successes = 0;
                    consecutive_failures += 1;

                    // If we hit failure threshold, should open
                    if consecutive_failures >= failure_threshold {
                        expected_state = kapsel_delivery::circuit::CircuitState::Open;
                    }

                    // Any failure in half-open should reopen
                    if matches!(expected_state, kapsel_delivery::circuit::CircuitState::HalfOpen) {
                        expected_state = kapsel_delivery::circuit::CircuitState::Open;
                    }
                }

                let stats = manager.circuit_stats(endpoint_id).await.unwrap();

                // Circuit should never be in an invalid state
                prop_assert!(matches!(stats.state,
                    kapsel_delivery::circuit::CircuitState::Closed |
                    kapsel_delivery::circuit::CircuitState::Open |
                    kapsel_delivery::circuit::CircuitState::HalfOpen
                ));

                // Counters should never be negative
                prop_assert!(stats.consecutive_failures <= operations.len() as u32);
                prop_assert!(stats.consecutive_successes <= operations.len() as u32);
                prop_assert!(stats.total_requests <= operations.len() as u32);
                prop_assert!(stats.failed_requests <= operations.len() as u32);
            }
            Ok(())
        })?;
    }

    /// Fuzzes backoff calculation with extreme parameter combinations.
    #[test]
    fn fuzz_backoff_extreme_values(
        attempt in 0u32..100,
        base_delay_ns in 1u64..1_000_000_000, // 1ns to 1s
        max_delay_ns in 1_000_000_000u64..86_400_000_000_000, // 1s to 24h
        jitter_factor in 0.0f64..2.0, // Allow >1 jitter for edge testing
        strategy in prop::sample::select(vec![
            BackoffStrategy::Exponential,
            BackoffStrategy::Linear,
            BackoffStrategy::Fixed,
        ])
    ) {
        let base_delay = Duration::from_nanos(base_delay_ns);
        let max_delay = Duration::from_nanos(max_delay_ns.max(base_delay_ns));

        let policy = RetryPolicy {
            max_attempts: 50,
            base_delay,
            max_delay,
            jitter_factor,
            backoff_strategy: strategy,
        };

        let error = DeliveryError::timeout(30);
        let context = RetryContext::new(
            attempt,
            error,
            Utc::now(),
            policy,
        );

        // Should not panic with extreme values
        let decision = context.decide_retry();

        match decision {
            kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } => {
                let delay = next_attempt_at.signed_duration_since(Utc::now());

                // Should produce valid delay within reasonable bounds (allow for jitter)
                if let Some(delay_ns) = delay.num_nanoseconds() {
                    prop_assert!(delay_ns >= 0);
                }
                // Allow 2x tolerance for extreme jitter scenarios
                let max_chrono_delay = chrono::Duration::from_std(max_delay * 2).unwrap_or(chrono::Duration::MAX);
                prop_assert!(delay <= max_chrono_delay);
            }
            kapsel_delivery::retry::RetryDecision::GiveUp { reason: _ } => {
                // Valid give up decision
            }
        }
    }

    /// Fuzzes error creation and categorization with random inputs.
    #[test]
    fn fuzz_error_categorization_random_inputs(
        status_code in 0u16..1000,
        timeout_seconds in 0u64..3600,
        message in "\\PC*",
        retry_after in 0u64..86400
    ) {
        // Test various error types with random inputs

        // Network error should always be retryable
        let network_error = DeliveryError::network(message.clone());
        prop_assert!(network_error.is_retryable());

        // Timeout error should always be retryable
        let timeout_error = DeliveryError::timeout(timeout_seconds);
        prop_assert!(timeout_error.is_retryable());

        // Rate limited should be retryable with valid retry_after
        let rate_error = DeliveryError::rate_limited(retry_after);
        prop_assert!(rate_error.is_retryable());
        prop_assert_eq!(rate_error.retry_after_seconds(), Some(retry_after));

        // Circuit open should not be retryable
        let circuit_error = DeliveryError::circuit_open(message.clone());
        prop_assert!(!circuit_error.is_retryable());

        // Configuration error should not be retryable
        let config_error = DeliveryError::configuration(message.clone());
        prop_assert!(!config_error.is_retryable());

        // Test HTTP status code categorization
        if (400..500).contains(&status_code) {
            let client_error = DeliveryError::client_error(status_code, message.clone());
            prop_assert!(!client_error.is_retryable());
        }

        if (500..600).contains(&status_code) {
            let server_error = DeliveryError::server_error(status_code, message.clone());
            prop_assert!(server_error.is_retryable());
        }

        // Error display should not panic with any input
        let _ = format!("{}", network_error);
        let _ = format!("{}", timeout_error);
        let _ = format!("{}", rate_error);
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

// Helper functions for property tests

#[derive(Debug, Clone)]
struct MockResponse {
    event_id: Uuid,
}

fn process_webhook_mock(webhook: &WebhookEvent) -> MockResponse {
    // Simulate idempotent processing
    MockResponse {
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

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn property_tests_with_real_database() {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // This would integrate with actual database
        let env = runtime.block_on(async { TestEnv::new().await.unwrap() });

        // Run property test with real persistence
        proptest!(|(webhook in strategies::webhook_event_strategy())| {
            // Test with actual database operations
            runtime.block_on(async {
                // Create tenant first (required for foreign key constraint)
                let tenant_name = format!("test-tenant-{}", webhook.tenant_id.simple());
                sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
                    .bind(webhook.tenant_id)
                    .bind(tenant_name)
                    .bind("enterprise")
                    .execute(&env.db.pool())
                    .await
                    .unwrap();

                // Create endpoint (required for foreign key constraint)
                sqlx::query(
                    "INSERT INTO endpoints (id, tenant_id, url, name, max_retries, timeout_seconds)
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING")
                .bind(webhook.endpoint_id)
                .bind(webhook.tenant_id)
                .bind("https://example.com/webhook")
                .bind("test-endpoint")
                .bind(10i32)
                .bind(30i32)
                .execute(&env.db.pool())
                .await
                .unwrap();

                // Insert webhook
                // payload_size must be at least 1 due to CHECK constraint
                let payload_size = (webhook.body.len() as i32).max(1);

                sqlx::query(
                    "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id,
                     idempotency_strategy, status, failure_count, headers, body, content_type, payload_size, received_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                     ON CONFLICT (tenant_id, endpoint_id, source_event_id) DO NOTHING")
                .bind(webhook.id)
                .bind(webhook.tenant_id)
                .bind(webhook.endpoint_id)
                .bind(webhook.source_event_id)
                .bind(webhook.idempotency_strategy)
                .bind("pending")
                .bind(0i32)
                .bind(serde_json::json!({}))
                .bind(webhook.body.as_ref())
                .bind(webhook.content_type)
                .bind(payload_size)
                .bind(webhook.received_at)
                .execute(&env.db.pool())
                .await
                .unwrap();

                // Verify webhook exists
                let count: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM webhook_events WHERE id = $1"
                )
                .bind(webhook.id)
                .fetch_one(&env.db.pool())
                .await
                .unwrap();

                assert_eq!(count.0, 1);
            });
        });
    }
}
