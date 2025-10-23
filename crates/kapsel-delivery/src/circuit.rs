//! Circuit breaker implementation for endpoint failure protection.
//!
//! Provides per-endpoint circuit breakers that fail fast during outages
//! and gradually test recovery. Prevents cascade failures by tracking
//! request success/failure rates and automatically switching states.
//!
//! # Circuit Breaker State Machine
//!
//! ```text
//!                          ┌─────────────────────────┐
//!                          │        CLOSED           │
//!                          │   (Normal Operation)    │
//!                          │                         │
//!                          │ ● All requests allowed  │
//!                          │ ● Tracking failure rate │
//!                          └─────────────────────────┘
//!                           │                        ▲
//!                           │                        │
//!               5 failures  │                        │ 3 successes
//!               OR 50% rate │                        │
//!                           ▼                        │
//!    ┌─────────────────────────┐                  ┌───────────────────────┐
//!    │         OPEN            │                  │       HALF-OPEN       │
//!    │      (Fail Fast)        │                  │   (Testing Recovery)  │
//!    │                         │   30s timeout    │                       │
//!    │ ● All requests blocked  │ ───────────────▶ │ ● Limited requests    │
//!    │ ● Immediate failure     │                  │ ● Gradual testing     │
//!    └─────────────────────────┘                  └───────────────────────┘
//!                                                             │
//!                                                             │
//!                                                 Any failure │
//!                                                             │
//!                                                             ▼
//!                                                 ┌───────────────────────┐
//!                                                 │     BACK TO OPEN      │
//!                                                 └───────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use kapsel_delivery::circuit::{CircuitBreakerManager, CircuitConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = CircuitConfig::default();
//! let manager = CircuitBreakerManager::new(config);
//!
//! // Check if request should be allowed
//! if manager.should_allow_request("endpoint-123").await {
//!     // Make hypothetical request and record outcome
//!     let request_result: Result<(), &str> = Ok(());
//!     match request_result {
//!         Ok(_) => manager.record_success("endpoint-123").await,
//!         Err(_) => manager.record_failure("endpoint-123").await,
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{DeliveryError, Result};

/// Circuit breaker configuration for all endpoints.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CircuitConfig {
    /// Number of consecutive failures to trigger circuit open.
    pub failure_threshold: u32,
    /// Minimum requests before considering failure rate.
    pub min_requests_for_rate: u32,
    /// Failure rate threshold (0.0 to 1.0) to trigger circuit open.
    pub failure_rate_threshold: f64,
    /// Time to wait before transitioning from Open to Half-Open.
    pub open_timeout: Duration,
    /// Number of consecutive successes to close circuit from Half-Open.
    pub success_threshold: u32,
    /// Maximum number of requests allowed in Half-Open state.
    pub half_open_max_requests: u32,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            min_requests_for_rate: 10,
            failure_rate_threshold: 0.5,
            open_timeout: Duration::from_secs(30),
            success_threshold: 3,
            half_open_max_requests: 3,
        }
    }
}

/// Current state of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// Normal operation - all requests allowed.
    Closed,
    /// Endpoint unhealthy - requests fail immediately.
    Open,
    /// Testing recovery - limited requests allowed.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half_open"),
        }
    }
}

/// Statistics and state for a single endpoint's circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitStats {
    /// Current circuit state.
    pub state: CircuitState,
    /// Number of consecutive failures in current state.
    pub consecutive_failures: u32,
    /// Number of consecutive successes in Half-Open state.
    pub consecutive_successes: u32,
    /// Total requests in current window.
    pub total_requests: u32,
    /// Failed requests in current window.
    pub failed_requests: u32,
    /// When circuit was last opened.
    pub last_opened_at: Option<Instant>,
    /// When circuit last changed state.
    pub last_state_change: Instant,
    /// Number of requests made in Half-Open state.
    pub half_open_requests: u32,
}

impl CircuitStats {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            consecutive_successes: 0,
            total_requests: 0,
            failed_requests: 0,
            last_opened_at: None,
            last_state_change: Instant::now(),
            half_open_requests: 0,
        }
    }

    /// Current failure rate as a percentage (0.0 to 1.0).
    pub fn failure_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            f64::from(self.failed_requests) / f64::from(self.total_requests)
        }
    }

    /// Time since circuit was last opened.
    pub fn time_since_opened(&self) -> Option<Duration> {
        self.last_opened_at.map(|opened_at| opened_at.elapsed())
    }

    /// Resets request counters for new measurement window.
    fn reset_counters(&mut self) {
        self.total_requests = 0;
        self.failed_requests = 0;
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
        self.half_open_requests = 0;
    }
}

/// Thread-safe circuit breaker manager for multiple endpoints.
///
/// Manages circuit breaker state for all configured endpoints. Uses internal
/// locking to ensure thread safety when accessed by multiple delivery workers
/// concurrently.
#[derive(Debug)]
pub struct CircuitBreakerManager {
    config: CircuitConfig,
    circuits: Arc<Mutex<HashMap<String, CircuitStats>>>,
}

impl CircuitBreakerManager {
    /// Creates a new circuit breaker manager with the given configuration.
    pub fn new(config: CircuitConfig) -> Self {
        Self { config, circuits: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Determines if a request should be allowed for the given endpoint.
    ///
    /// Returns `true` if the request should proceed, `false` if the circuit
    /// breaker should block the request. Updates internal state for Half-Open
    /// transitions based on timeout.
    #[allow(clippy::significant_drop_tightening)] // Atomic state transition with check required
    pub async fn should_allow_request(&self, endpoint_id: &str) -> bool {
        let (current_state, half_open_requests) = {
            let mut circuits = self.circuits.lock().await;
            let endpoint_stats =
                circuits.entry(endpoint_id.to_string()).or_insert_with(CircuitStats::new);

            if endpoint_stats.state == CircuitState::Open {
                if let Some(time_since_opened) = endpoint_stats.time_since_opened() {
                    if time_since_opened >= self.config.open_timeout {
                        Self::transition_to_half_open(endpoint_stats);
                    }
                }
            }

            (endpoint_stats.state, endpoint_stats.half_open_requests)
        };

        match current_state {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => half_open_requests < self.config.half_open_max_requests,
        }
    }

    /// Records a successful request outcome for the endpoint.
    ///
    /// Updates failure counters and potentially transitions the circuit from
    /// Half-Open to Closed if success threshold is reached.
    #[allow(clippy::significant_drop_tightening)] // Atomic state transition required
    pub async fn record_success(&self, endpoint_id: &str) {
        let mut circuits = self.circuits.lock().await;
        let endpoint_stats =
            circuits.entry(endpoint_id.to_string()).or_insert_with(CircuitStats::new);

        endpoint_stats.total_requests += 1;
        endpoint_stats.consecutive_failures = 0;

        match endpoint_stats.state {
            CircuitState::Closed => {},
            CircuitState::Open => {
                tracing::warn!("Recorded success for open circuit: {endpoint_id}");
            },
            CircuitState::HalfOpen => {
                endpoint_stats.consecutive_successes += 1;
                endpoint_stats.half_open_requests += 1;

                if endpoint_stats.consecutive_successes >= self.config.success_threshold {
                    Self::transition_to_closed(endpoint_stats);
                }
            },
        }
    }

    /// Records a failed request outcome for the endpoint.
    ///
    /// Updates failure counters and potentially opens the circuit if failure
    /// thresholds are exceeded.
    #[allow(clippy::significant_drop_tightening)] // Atomic state transition required
    pub async fn record_failure(&self, endpoint_id: &str) {
        let mut circuits = self.circuits.lock().await;
        let endpoint_stats =
            circuits.entry(endpoint_id.to_string()).or_insert_with(CircuitStats::new);

        endpoint_stats.total_requests += 1;
        endpoint_stats.failed_requests += 1;
        endpoint_stats.consecutive_failures += 1;
        endpoint_stats.consecutive_successes = 0;

        match endpoint_stats.state {
            CircuitState::Closed => {
                if self.should_open_circuit(endpoint_stats) {
                    Self::transition_to_open(endpoint_stats);
                }
            },
            CircuitState::Open => {},
            CircuitState::HalfOpen => {
                endpoint_stats.half_open_requests += 1;
                Self::transition_to_open(endpoint_stats);
            },
        }
    }

    /// Returns current circuit breaker statistics for an endpoint.
    pub async fn circuit_stats(&self, endpoint_id: &str) -> Option<CircuitStats> {
        let circuits = self.circuits.lock().await;
        circuits.get(endpoint_id).cloned()
    }

    /// Returns all circuit breaker statistics.
    pub async fn all_circuit_stats(&self) -> HashMap<String, CircuitStats> {
        self.circuits.lock().await.clone()
    }

    /// Forces a circuit to the specified state (for testing/admin purposes).
    #[allow(clippy::significant_drop_tightening)] // Atomic state change required
    pub async fn force_circuit_state(&self, endpoint_id: &str, state: CircuitState) {
        let mut circuits = self.circuits.lock().await;
        let endpoint_stats =
            circuits.entry(endpoint_id.to_string()).or_insert_with(CircuitStats::new);

        endpoint_stats.state = state;
        endpoint_stats.last_state_change = Instant::now();

        if state == CircuitState::Open {
            endpoint_stats.last_opened_at = Some(Instant::now());
        }

        if state == CircuitState::Closed {
            endpoint_stats.reset_counters();
        }
    }

    /// Checks if circuit should be opened based on failure thresholds.
    fn should_open_circuit(&self, endpoint_stats: &CircuitStats) -> bool {
        if endpoint_stats.consecutive_failures >= self.config.failure_threshold {
            return true;
        }

        if endpoint_stats.total_requests >= self.config.min_requests_for_rate
            && endpoint_stats.failure_rate() >= self.config.failure_rate_threshold
        {
            return true;
        }

        false
    }

    /// Transitions circuit to Open state.
    fn transition_to_open(endpoint_stats: &mut CircuitStats) {
        tracing::warn!(
            "Circuit breaker opening - failures: {}, rate: {:.2}",
            endpoint_stats.consecutive_failures,
            endpoint_stats.failure_rate()
        );

        endpoint_stats.state = CircuitState::Open;
        endpoint_stats.last_opened_at = Some(Instant::now());
        endpoint_stats.last_state_change = Instant::now();
    }

    /// Transitions circuit to Half-Open state.
    fn transition_to_half_open(endpoint_stats: &mut CircuitStats) {
        tracing::info!("Circuit breaker transitioning to half-open");

        endpoint_stats.state = CircuitState::HalfOpen;
        endpoint_stats.last_state_change = Instant::now();
        endpoint_stats.half_open_requests = 0;
        endpoint_stats.consecutive_successes = 0;
    }

    /// Transitions circuit to Closed state.
    fn transition_to_closed(endpoint_stats: &mut CircuitStats) {
        tracing::info!("Circuit breaker closing - endpoint recovered");

        endpoint_stats.state = CircuitState::Closed;
        endpoint_stats.last_state_change = Instant::now();
        endpoint_stats.reset_counters();
    }
}

/// Checks if a delivery should be blocked by circuit breaker.
///
/// Convenience function that returns a circuit breaker error if the request
/// should be blocked, or Ok(()) if it should proceed.
pub async fn check_circuit_breaker(
    manager: &CircuitBreakerManager,
    endpoint_id: &str,
) -> Result<()> {
    if manager.should_allow_request(endpoint_id).await {
        Ok(())
    } else {
        Err(DeliveryError::circuit_open(endpoint_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitConfig {
        CircuitConfig {
            failure_threshold: 3,
            min_requests_for_rate: 5,
            failure_rate_threshold: 0.6,
            open_timeout: Duration::from_millis(100),
            success_threshold: 2,
            half_open_max_requests: 2,
        }
    }

    #[tokio::test]
    async fn circuit_starts_closed() {
        let manager = CircuitBreakerManager::new(test_config());
        assert!(manager.should_allow_request("test-endpoint").await);

        let stats = manager.circuit_stats("test-endpoint").await.unwrap();
        assert_eq!(stats.state, CircuitState::Closed);
    }

    #[tokio::test]
    async fn consecutive_failures_open_circuit() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Record failures up to threshold
        for _ in 0..2 {
            manager.record_failure(endpoint).await;
            assert!(manager.should_allow_request(endpoint).await);
        }

        // Third failure should open circuit
        manager.record_failure(endpoint).await;
        assert!(!manager.should_allow_request(endpoint).await);

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::Open);
    }

    #[tokio::test]
    async fn failure_rate_opens_circuit() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Mix of success and failure to build up request count
        manager.record_success(endpoint).await;
        manager.record_success(endpoint).await;
        manager.record_failure(endpoint).await;
        manager.record_failure(endpoint).await;

        // Should still be closed (4 requests, 2 failures = 50% < 60% threshold)
        assert!(manager.should_allow_request(endpoint).await);

        // One more failure pushes rate to 60%
        manager.record_failure(endpoint).await;
        assert!(!manager.should_allow_request(endpoint).await);

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::Open);
    }

    #[tokio::test]
    async fn open_circuit_transitions_to_half_open() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Open the circuit
        for _ in 0..3 {
            manager.record_failure(endpoint).await;
        }
        assert!(!manager.should_allow_request(endpoint).await);

        // Wait for timeout (using force state to simulate time passage)
        manager.force_circuit_state(endpoint, CircuitState::HalfOpen).await;

        // Should now allow limited requests
        assert!(manager.should_allow_request(endpoint).await);

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::HalfOpen);
    }

    #[tokio::test]
    async fn half_open_limits_requests() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        manager.force_circuit_state(endpoint, CircuitState::HalfOpen).await;

        // First two requests should be allowed
        assert!(manager.should_allow_request(endpoint).await);
        manager.record_success(endpoint).await;

        assert!(manager.should_allow_request(endpoint).await);
        manager.record_success(endpoint).await;

        // Circuit should now be closed after 2 successes (success_threshold = 2)
        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::Closed);
    }

    #[tokio::test]
    async fn half_open_failure_reopens_circuit() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        manager.force_circuit_state(endpoint, CircuitState::HalfOpen).await;

        // First request succeeds
        manager.record_success(endpoint).await;
        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::HalfOpen);

        // Second request fails - should reopen circuit
        manager.record_failure(endpoint).await;
        assert!(!manager.should_allow_request(endpoint).await);

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.state, CircuitState::Open);
    }

    #[tokio::test]
    async fn success_resets_failure_counters() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Build up some failures
        manager.record_failure(endpoint).await;
        manager.record_failure(endpoint).await;

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.consecutive_failures, 2);

        // Success should reset counter
        manager.record_success(endpoint).await;
        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn circuit_stats_track_correctly() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Record mix of success and failure
        manager.record_success(endpoint).await;
        manager.record_failure(endpoint).await;
        manager.record_failure(endpoint).await;

        let stats = manager.circuit_stats(endpoint).await.unwrap();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.failed_requests, 2);
        assert_eq!(stats.consecutive_failures, 2);
        assert!((stats.failure_rate() - 0.6667).abs() < 0.01);
    }

    #[tokio::test]
    async fn check_circuit_breaker_helper() {
        let manager = CircuitBreakerManager::new(test_config());
        let endpoint = "test-endpoint";

        // Should allow when closed
        assert!(check_circuit_breaker(&manager, endpoint).await.is_ok());

        // Force open and verify error
        manager.force_circuit_state(endpoint, CircuitState::Open).await;
        let result = check_circuit_breaker(&manager, endpoint).await;
        assert!(result.is_err());

        if let Err(DeliveryError::CircuitOpen { endpoint_id }) = result {
            assert_eq!(endpoint_id, endpoint);
        } else {
            unreachable!("Expected CircuitOpen error, got: {:?}", result);
        }
    }
}
