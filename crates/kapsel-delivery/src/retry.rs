//! Exponential backoff retry strategies with jitter.
//!
//! Implements configurable retry policies for failed webhook deliveries.
//! Includes exponential backoff timing, jitter for load distribution,
//! and integration with circuit breaker failure detection.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::error::DeliveryError;

/// Retry policy configuration for webhook delivery.
///
/// Defines how delivery failures should be retried, including backoff strategy,
/// maximum attempts, and timeout limits. Policies can be customized per
/// endpoint to handle different destination characteristics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of delivery attempts (including initial attempt).
    pub max_attempts: u32,

    /// Base delay for exponential backoff calculation.
    pub base_delay: Duration,

    /// Maximum delay between retry attempts.
    pub max_delay: Duration,

    /// Jitter percentage (0.0 to 1.0) to add randomness.
    pub jitter_factor: f64,

    /// Strategy for calculating backoff delays.
    pub backoff_strategy: BackoffStrategy,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(512),
            jitter_factor: 0.25, // ±25% randomization
            backoff_strategy: BackoffStrategy::Exponential,
        }
    }
}

/// Strategy for calculating retry delays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackoffStrategy {
    /// Fixed delay between retries.
    Fixed,
    /// Exponential backoff: delay doubles each attempt.
    Exponential,
    /// Linear backoff: delay increases by base amount each attempt.
    Linear,
}

/// Retry decision context for a failed delivery attempt.
///
/// Contains all information needed to determine if a delivery should be
/// retried and when the next attempt should occur.
#[derive(Debug, Clone)]
pub struct RetryContext {
    /// Current attempt number (1-based).
    pub attempt_number: u32,
    /// Error that caused the delivery failure.
    pub error: DeliveryError,
    /// Timestamp of the failed attempt.
    pub failed_at: DateTime<Utc>,
    /// Retry policy to apply.
    pub policy: RetryPolicy,
}

/// Result of retry decision calculation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry the delivery at the specified time.
    Retry {
        /// When the next delivery attempt should be made
        next_attempt_at: DateTime<Utc>,
    },
    /// Do not retry - delivery permanently failed.
    GiveUp {
        /// Reason why the delivery should not be retried
        reason: String,
    },
}

impl RetryContext {
    /// Creates a new retry context for a failed delivery.
    pub fn new(
        attempt_number: u32,
        error: DeliveryError,
        failed_at: DateTime<Utc>,
        policy: RetryPolicy,
    ) -> Self {
        Self { attempt_number, error, failed_at, policy }
    }

    /// Determines if and when to retry based on the failure context.
    ///
    /// Considers the error type, attempt count, and policy configuration to
    /// make retry decisions. Respects HTTP 429 Retry-After headers and
    /// prevents retries for non-retryable errors.
    pub fn decide_retry(&self) -> RetryDecision {
        if self.attempt_number >= self.policy.max_attempts {
            return RetryDecision::GiveUp {
                reason: format!("maximum attempts ({}) exceeded", self.policy.max_attempts),
            };
        }

        if !self.error.is_retryable() {
            return RetryDecision::GiveUp {
                reason: format!("non-retryable error: {}", self.error),
            };
        }

        let delay = self.calculate_delay();
        let Ok(chrono_delay) = chrono::Duration::from_std(delay) else {
            return RetryDecision::GiveUp {
                reason: "retry delay duration out of range".to_string(),
            };
        };
        let next_attempt_at = self.failed_at + chrono_delay;

        RetryDecision::Retry { next_attempt_at }
    }

    /// Calculates the delay until the next retry attempt.
    ///
    /// Uses the configured backoff strategy with jitter to determine wait time.
    /// For rate limit errors (HTTP 429), respects the Retry-After header.
    fn calculate_delay(&self) -> Duration {
        if let Some(retry_after_seconds) = self.error.retry_after_seconds() {
            return Duration::from_secs(retry_after_seconds);
        }

        let base_delay = match self.policy.backoff_strategy {
            BackoffStrategy::Fixed => self.policy.base_delay,
            BackoffStrategy::Linear => {
                self.policy.base_delay * self.attempt_number.saturating_sub(1)
            },
            BackoffStrategy::Exponential => {
                let exponent = self.attempt_number.saturating_sub(1).min(20);
                let multiplier = 2_u32.saturating_pow(exponent);
                self.policy.base_delay * multiplier
            },
        };

        let capped_delay = std::cmp::min(base_delay, self.policy.max_delay);

        let jittered_delay = apply_jitter(capped_delay, self.policy.jitter_factor);

        std::cmp::min(jittered_delay, self.policy.max_delay)
    }
}

/// Applies jitter to a duration to prevent thundering herd effects.
///
/// Randomizes the delay by ±jitter_factor percentage. For example, with
/// jitter_factor=0.25, a 10s delay becomes 7.5s to 12.5s randomly.
fn apply_jitter(duration: Duration, jitter_factor: f64) -> Duration {
    if jitter_factor <= 0.0 {
        return duration;
    }

    let clamped_jitter = jitter_factor.clamp(0.0, 1.0);

    let mut rng = rand::rng();
    let jitter_range = duration.as_secs_f64() * clamped_jitter;
    let jitter_offset = rng.random_range(-jitter_range..=jitter_range);
    let jittered_secs = duration.as_secs_f64() + jitter_offset;

    Duration::from_secs_f64(jittered_secs.max(0.0))
}

/// Creates a retry policy with exponential backoff matching specification.
///
/// Default policy: 10 attempts, 1s base delay, 512s max delay, ±25% jitter.
/// Produces delays: ~1s, ~2s, ~4s, ~8s, ~16s, ~32s, ~64s, ~128s, ~256s, ~512s
pub fn default_exponential_policy() -> RetryPolicy {
    RetryPolicy::default()
}

/// Creates a retry policy optimized for fast APIs with short timeouts.
pub fn fast_api_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(30),
        jitter_factor: 0.1,
        backoff_strategy: BackoffStrategy::Exponential,
    }
}

/// Creates a retry policy for batch processing with longer delays.
pub fn batch_processing_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 15,
        base_delay: Duration::from_secs(5),
        max_delay: Duration::from_secs(3600), // 1 hour
        jitter_factor: 0.5,
        backoff_strategy: BackoffStrategy::Exponential,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_increases_correctly() {
        let policy = default_exponential_policy();
        let base_time = Utc::now();

        // Test progression without jitter for predictability
        let mut policy_no_jitter = policy;
        policy_no_jitter.jitter_factor = 0.0;

        let delays = (1..=5)
            .map(|attempt| {
                let context = RetryContext::new(
                    attempt,
                    DeliveryError::timeout(30),
                    base_time,
                    policy_no_jitter.clone(),
                );
                context.calculate_delay()
            })
            .collect::<Vec<_>>();

        // Should be: 1s, 2s, 4s, 8s, 16s
        assert_eq!(delays[0], Duration::from_secs(1));
        assert_eq!(delays[1], Duration::from_secs(2));
        assert_eq!(delays[2], Duration::from_secs(4));
        assert_eq!(delays[3], Duration::from_secs(8));
        assert_eq!(delays[4], Duration::from_secs(16));
    }

    #[test]
    fn retry_respects_maximum_attempts() {
        let policy = RetryPolicy { max_attempts: 3, ..Default::default() };

        let context = RetryContext::new(
            3, // At maximum attempts
            DeliveryError::timeout(30),
            Utc::now(),
            policy,
        );

        match context.decide_retry() {
            RetryDecision::GiveUp { reason } => {
                assert!(reason.contains("maximum attempts"));
            },
            RetryDecision::Retry { .. } => {
                unreachable!("Should not retry when at max attempts");
            },
        }
    }

    #[test]
    fn non_retryable_errors_rejected() {
        let context = RetryContext::new(
            1,
            DeliveryError::client_error(404, "not found"),
            Utc::now(),
            default_exponential_policy(),
        );

        match context.decide_retry() {
            RetryDecision::GiveUp { reason } => {
                assert!(reason.contains("non-retryable"));
            },
            RetryDecision::Retry { .. } => {
                unreachable!("Should not retry client errors");
            },
        }
    }

    #[test]
    fn retry_after_header_respected() {
        let context = RetryContext::new(
            1,
            DeliveryError::rate_limited(120), // 2 minutes
            Utc::now(),
            default_exponential_policy(),
        );

        let delay = context.calculate_delay();
        assert_eq!(delay, Duration::from_secs(120));
    }

    #[test]
    fn jitter_varies_delay() {
        let policy = RetryPolicy {
            jitter_factor: 0.5, // Large jitter for testing
            ..Default::default()
        };

        let base_delay = Duration::from_secs(10);
        let mut seen_delays = std::collections::HashSet::new();

        // Generate multiple jittered delays - should vary
        for _ in 0..20 {
            let jittered = apply_jitter(base_delay, policy.jitter_factor);
            seen_delays.insert(jittered.as_millis());
        }

        // Should see multiple different values due to randomization
        assert!(seen_delays.len() > 1, "Jitter should create variation");

        // All values should be reasonable (5-15 seconds with 50% jitter)
        for &delay_ms in &seen_delays {
            assert!(delay_ms >= 5_000, "Delay too small: {delay_ms}ms");
            assert!(delay_ms <= 15_000, "Delay too large: {delay_ms}ms");
        }
    }

    #[test]
    fn max_delay_enforced() {
        let policy = RetryPolicy {
            max_delay: Duration::from_secs(60),
            jitter_factor: 0.0, // No jitter for predictable test
            ..Default::default()
        };

        let context = RetryContext::new(
            10, // High attempt number for large exponential delay
            DeliveryError::timeout(30),
            Utc::now(),
            policy,
        );

        let delay = context.calculate_delay();
        assert!(delay <= Duration::from_secs(60));
    }

    #[test]
    fn linear_backoff_strategy() {
        let policy = RetryPolicy {
            backoff_strategy: BackoffStrategy::Linear,
            base_delay: Duration::from_secs(5),
            jitter_factor: 0.0,
            ..Default::default()
        };

        let delays = (1..=4)
            .map(|attempt| {
                let context = RetryContext::new(
                    attempt,
                    DeliveryError::timeout(30),
                    Utc::now(),
                    policy.clone(),
                );
                context.calculate_delay()
            })
            .collect::<Vec<_>>();

        // Should be: 0s, 5s, 10s, 15s (linear increase)
        assert_eq!(delays[0], Duration::from_secs(0));
        assert_eq!(delays[1], Duration::from_secs(5));
        assert_eq!(delays[2], Duration::from_secs(10));
        assert_eq!(delays[3], Duration::from_secs(15));
    }

    #[test]
    fn fixed_backoff_strategy() {
        let policy = RetryPolicy {
            backoff_strategy: BackoffStrategy::Fixed,
            base_delay: Duration::from_secs(10),
            jitter_factor: 0.0,
            ..Default::default()
        };

        let delays = (1..=5)
            .map(|attempt| {
                let context = RetryContext::new(
                    attempt,
                    DeliveryError::timeout(30),
                    Utc::now(),
                    policy.clone(),
                );
                context.calculate_delay()
            })
            .collect::<Vec<_>>();

        // All delays should be the same
        for delay in delays {
            assert_eq!(delay, Duration::from_secs(10));
        }
    }

    #[test]
    fn preset_policies_have_reasonable_values() {
        let fast = fast_api_policy();
        assert!(fast.max_attempts <= 10);
        assert!(fast.base_delay <= Duration::from_secs(1));

        let batch = batch_processing_policy();
        assert!(batch.max_attempts >= 10);
        assert!(batch.base_delay >= Duration::from_secs(1));
        assert!(batch.max_delay >= Duration::from_secs(300));
    }
}
