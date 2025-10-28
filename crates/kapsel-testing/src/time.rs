//! Deterministic time control and clock utilities for testing.
//!
//! Provides controllable time progression, duration measurement, and
//! timestamp generation for reproducible time-based test scenarios.

use std::{
    cmp::min,
    sync::Arc,
    time::{Duration, Instant},
};

// Re-export TestClock from kapsel-core for convenience
pub use kapsel_core::time::TestClock;
use kapsel_core::Clock;
use rand::Rng;

/// Timer for measuring durations with test clock support.
pub struct Timer<C: Clock> {
    start: Instant,
    clock: Arc<C>,
}

impl<C: Clock> Timer<C> {
    /// Starts a new timer.
    pub fn start(clock: Arc<C>) -> Self {
        Self { start: clock.now(), clock }
    }

    /// Returns elapsed time since timer started.
    pub fn elapsed(&self) -> Duration {
        self.clock.now().duration_since(self.start)
    }

    /// Resets the timer to current time.
    pub fn reset(&mut self) {
        self.start = self.clock.now();
    }
}

/// Utilities for retry timing calculations.
pub mod backoff {
    use super::{min, Duration, Rng};

    /// Calculates exponential backoff with jitter.
    ///
    /// Jitter prevents thundering herd when multiple workers retry
    /// simultaneously.
    pub fn exponential_with_jitter(
        attempt: u32,
        base: Duration,
        max: Duration,
        jitter_factor: f32,
    ) -> Duration {
        // Cap attempt to prevent overflow
        let capped_attempt = min(attempt, 10);
        let base_delay = base * 2u32.pow(capped_attempt);
        let capped_delay = min(base_delay, max);

        // Add jitter: ±jitter_factor of base delay
        let delay_ms =
            u64::try_from(capped_delay.as_millis().min(u128::from(u64::MAX))).unwrap_or(0);
        let jitter_range = if delay_ms == 0 {
            0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let jitter_calc = delay_ms as f64 * f64::from(jitter_factor);
            if jitter_calc.is_finite() && jitter_calc >= 0.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let jitter_u64 = jitter_calc as u64;
                jitter_u64.min(delay_ms)
            } else {
                0
            }
        };
        let jitter =
            if jitter_range > 0 { rand::rng().random_range(0..jitter_range * 2) } else { 0 };
        let jittered_ms = delay_ms.saturating_sub(jitter_range).saturating_add(jitter);

        Duration::from_millis(jittered_ms)
    }

    /// Standard backoff for webhook retries.
    pub fn standard_webhook_backoff(attempt: u32) -> Duration {
        exponential_with_jitter(attempt, Duration::from_secs(1), Duration::from_secs(512), 0.25)
    }

    /// Deterministic exponential backoff without jitter for tests.
    pub fn deterministic_webhook_backoff(attempt: u32) -> Duration {
        let base = Duration::from_secs(1);
        let capped_attempt = min(attempt, 10);
        let delay = base * 2u32.pow(capped_attempt);
        min(delay, Duration::from_secs(512))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_exponentially() {
        use backoff::standard_webhook_backoff;

        let d1 = standard_webhook_backoff(0);
        let d2 = standard_webhook_backoff(1);
        let d3 = standard_webhook_backoff(2);

        // Should roughly double each time (with jitter)
        assert!(d2 > d1);
        assert!(d3 > d2);
        assert!(d1 >= Duration::from_millis(750)); // 1s - 25% jitter
        assert!(d1 <= Duration::from_millis(1250)); // 1s + 25% jitter
    }

    #[test]
    fn backoff_respects_max() {
        use backoff::exponential_with_jitter;

        let max = Duration::from_secs(10);
        let result = exponential_with_jitter(100, Duration::from_secs(1), max, 0.0);

        assert_eq!(result, max);
    }

    #[tokio::test]
    async fn test_timer_with_test_clock() {
        let clock = Arc::new(TestClock::new());
        let mut timer = Timer::start(clock.clone());

        clock.advance(Duration::from_secs(5));

        assert_eq!(timer.elapsed(), Duration::from_secs(5));

        timer.reset();
        clock.advance(Duration::from_secs(10));

        assert_eq!(timer.elapsed(), Duration::from_secs(10));
    }
}
