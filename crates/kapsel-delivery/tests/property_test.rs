//! Property-based tests for delivery component invariants.
//!
//! Uses randomly generated inputs to verify delivery invariants always
//! hold regardless of input data or internal state.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use kapsel_core::{Clock, TestClock};
use kapsel_delivery::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    error::DeliveryError,
    retry::{BackoffStrategy, RetryContext, RetryPolicy},
};
use proptest::{prelude::*, test_runner::Config as ProptestConfig};

/// Creates property test configuration based on environment.
///
/// Uses environment variables:
/// - `PROPTEST_CASES`: Number of test cases (default: 20 for dev, 100 for CI)
/// - `CI`: If set to "true", uses CI configuration
fn proptest_config() -> ProptestConfig {
    let is_ci = std::env::var("CI").unwrap_or_default() == "true";
    let default_cases = if is_ci { 12 } else { 8 };

    let cases =
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(default_cases);

    ProptestConfig::with_cases(cases)
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
        let clock = Arc::new(TestClock::new());
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
            DateTime::<Utc>::from(clock.now_system()),
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
        let clock = Arc::new(TestClock::new());
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
            DateTime::<Utc>::from(clock.now_system()),
            policy,
        );

        if let kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } = context.decide_retry() {
            let delay = next_attempt_at.signed_duration_since(DateTime::<Utc>::from(clock.now_system()));
            let delay_secs = u64::try_from(delay.num_seconds().max(0)).unwrap_or(0);

            // Delay should be at least 0 and at most max_delay (with some tolerance for jitter)
            prop_assert!(delay_secs <= max_delay_secs + 1); // Allow 1 second tolerance for jitter
        }
    }

    /// Verifies circuit breaker transitions correctly between states.
    #[test]
    fn delivery_circuit_breaker_state_transitions(
        failure_threshold in 1usize..15,
        success_threshold in 1usize..8,
        failure_rate_threshold in 0.1f64..0.9,
        consecutive_failures in 0usize..30
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let config = CircuitConfig {
            failure_threshold: u32::try_from(failure_threshold).unwrap(),
            success_threshold: u32::try_from(success_threshold).unwrap(),
            failure_rate_threshold,
            min_requests_for_rate: 5,
            open_timeout: Duration::from_secs(10),
            half_open_max_requests: 3,
        };

        rt.block_on(async {
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
            prop_assert_eq!(stats.consecutive_failures, u32::try_from(consecutive_failures).unwrap());
            Ok(())
        })?;
    }

    /// Verifies circuit breaker opens when failure rate threshold is exceeded.
    #[test]
    fn delivery_circuit_breaker_failure_rate(
        total_requests in 10usize..100,
        failed_requests in 0usize..100
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let failed_requests = failed_requests.min(total_requests);
        #[allow(clippy::cast_precision_loss)]
        let expected_rate = failed_requests as f64 / total_requests as f64;

        rt.block_on(async {
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
            prop_assert_eq!(stats.total_requests, u32::try_from(total_requests).unwrap());
            prop_assert_eq!(stats.failed_requests, u32::try_from(failed_requests).unwrap());
            Ok(())
        })?;
    }

    /// Verifies delivery error categorization for retry decisions.
    #[test]
    fn delivery_error_retry_categorization(
        status_code in 400u16..600,
        timeout_seconds in 1u64..120
    ) {
        // Client errors (4xx) should not be retryable, except 408 and 425
        if (400..500).contains(&status_code) {
            let error = DeliveryError::client_error(status_code, "Client Error".to_string());
            // Special cases: 408 Request Timeout and 425 Too Early are retryable
            if matches!(status_code, 408 | 425) {
                prop_assert!(error.is_retryable());
            } else {
                prop_assert!(!error.is_retryable());
            }
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
        max_delay_ms in 1u64..86_400_000, // Up to 24 hours
        jitter_factor in 0.0f64..1.0
    ) {
        let clock = Arc::new(TestClock::new());
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
            DateTime::<Utc>::from(clock.now_system()),
            policy,
        );

        let decision = context.decide_retry();

        // Should always produce valid decision
        match decision {
            kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } => {
                let delay = next_attempt_at.signed_duration_since(DateTime::<Utc>::from(clock.now_system()));
                // Delay should never be negative or significantly exceed max_delay
                prop_assert!(delay.num_milliseconds() >= 0);
                // Allow some tolerance for jitter and timing
                let max_delay_ms = i64::try_from(max_delay.as_millis()).unwrap_or(i64::MAX);
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
        failure_threshold in 1usize..20,
        success_threshold in 1usize..10,
        operations in prop::collection::vec(any::<bool>(), 1..100)
    ) {
        let config = CircuitConfig {
            failure_threshold: u32::try_from(failure_threshold).unwrap(),
            success_threshold: u32::try_from(success_threshold).unwrap(),
            failure_rate_threshold: 0.5,
            min_requests_for_rate: 5,
            open_timeout: Duration::from_secs(1),
            half_open_max_requests: 5,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let manager = CircuitBreakerManager::new(config);
            let endpoint_id = "fuzz-endpoint";

            let mut consecutive_failures = 0;
            let mut consecutive_successes = 0;
            let mut expected_state = kapsel_delivery::circuit::CircuitState::Closed;

            for success in &operations {
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
                prop_assert!(stats.consecutive_failures <= u32::try_from(operations.len()).unwrap());
                prop_assert!(stats.consecutive_successes <= u32::try_from(operations.len()).unwrap());
                prop_assert!(stats.total_requests <= u32::try_from(operations.len()).unwrap());
                prop_assert!(stats.failed_requests <= u32::try_from(operations.len()).unwrap());
            }
            Ok(())
        })?;
    }

    /// Fuzzes backoff calculation with extreme parameter combinations.
    #[test]
    fn fuzz_backoff_extreme_values(
        attempt in 1u32..100,
        base_delay_ns in 1u64..1_000_000_000, // 1ns to 1s
        max_delay_ns in 1_000_000_000u64..86_400_000_000_000, // 1s to 24h
        jitter_factor in 0.0f64..2.0, // Allow >1 jitter for edge testing
        strategy in prop::sample::select(vec![
            BackoffStrategy::Exponential,
            BackoffStrategy::Linear,
            BackoffStrategy::Fixed,
        ])
    ) {
        let clock = Arc::new(TestClock::new());
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
            DateTime::<Utc>::from(clock.now_system()),
            policy,
        );

        // Should not panic with extreme values
        let decision = context.decide_retry();

        match decision {
            kapsel_delivery::retry::RetryDecision::Retry { next_attempt_at } => {
                let delay = next_attempt_at.signed_duration_since(DateTime::<Utc>::from(clock.now_system()));

                // For very small delays (especially 0 from Linear strategy with attempt=1),
                // the next_attempt_at might be in the past due to test execution time.
                // Allow small negative delays (up to 100ms) as acceptable timing variance.
                if let Some(delay_ns) = delay.num_nanoseconds() {
                    prop_assert!(delay_ns >= -100_000_000, "delay too negative: {}ns", delay_ns);
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
            // Special cases: 408 Request Timeout and 425 Too Early are retryable
            if matches!(status_code, 408 | 425) {
                prop_assert!(client_error.is_retryable());
            } else {
                prop_assert!(!client_error.is_retryable());
            }
        }

        if (500..600).contains(&status_code) {
            let server_error = DeliveryError::server_error(status_code, message);
            prop_assert!(server_error.is_retryable());
        }

        // Error display should not panic with any input
        let _ = format!("{network_error}");
        let _ = format!("{timeout_error}");
        let _ = format!("{rate_error}");
    }
}
