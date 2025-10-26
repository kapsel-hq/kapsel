//! Deterministic time control and clock utilities for testing.
//!
//! Provides controllable time progression, duration measurement, and
//! timestamp generation for reproducible time-based test scenarios.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use kapsel_core::Clock;
use rand::Rng;

/// Test clock for deterministic time control.
#[derive(Clone)]
pub struct TestClock {
    /// Monotonic time in nanoseconds since start
    monotonic_ns: Arc<AtomicU64>,
    /// System time as nanoseconds since UNIX_EPOCH
    system_ns: Arc<AtomicU64>,
    /// Base instant for monotonic time calculations
    base_instant: Instant,
}

impl TestClock {
    /// Creates a new test clock starting at current time.
    pub fn new() -> Self {
        let now = SystemTime::now();
        let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();

        Self {
            monotonic_ns: Arc::new(AtomicU64::new(0)),
            system_ns: Arc::new(AtomicU64::new(
                u64::try_from(since_epoch.as_nanos().min(u128::from(u64::MAX))).unwrap_or(0),
            )),
            base_instant: Instant::now(),
        }
    }

    /// Creates a test clock starting at a specific time.
    pub fn with_start_time(start: SystemTime) -> Self {
        let since_epoch = start.duration_since(UNIX_EPOCH).unwrap_or_default();

        Self {
            monotonic_ns: Arc::new(AtomicU64::new(0)),
            system_ns: Arc::new(AtomicU64::new(
                u64::try_from(since_epoch.as_nanos().min(u128::from(u64::MAX))).unwrap_or(0),
            )),
            base_instant: Instant::now(),
        }
    }

    /// Returns the current instant in test time.
    pub fn now_instant(&self) -> Instant {
        let elapsed_ns = self.monotonic_ns.load(Ordering::Acquire);
        self.base_instant + Duration::from_nanos(elapsed_ns)
    }

    /// Returns the current system time.
    pub fn now_system(&self) -> SystemTime {
        let ns = self.system_ns.load(Ordering::Acquire);
        UNIX_EPOCH + Duration::from_nanos(ns)
    }

    /// Advances both clocks by the specified duration.
    pub fn advance(&self, duration: Duration) {
        let duration_ns = u64::try_from(duration.as_nanos().min(u128::from(u64::MAX))).unwrap_or(0);

        // Update monotonic time and system time
        self.monotonic_ns.fetch_add(duration_ns, Ordering::AcqRel);
        self.system_ns.fetch_add(duration_ns, Ordering::AcqRel);
    }

    /// Jumps the clock to a specific system time.
    pub fn jump_to(&self, time: SystemTime) {
        let target_ns = u64::try_from(
            time.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .min(u128::from(u64::MAX)),
        )
        .unwrap_or(0);
        let current_ns = self.system_ns.load(Ordering::Acquire);

        if target_ns > current_ns {
            let diff_ns = target_ns - current_ns;
            self.advance(Duration::from_nanos(diff_ns));
        } else {
            // System time is allowed to jump backwards (monotonic stays forward)
            self.system_ns.store(target_ns, Ordering::Release);
        }
    }

    /// Returns elapsed time since clock creation.
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.monotonic_ns.load(Ordering::Acquire))
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.now_instant()
    }

    fn now_system(&self) -> SystemTime {
        self.now_system()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        // In tests, sleep just advances the clock
        self.advance(duration);
        // Yield to allow other tasks to run
        Box::pin(tokio::task::yield_now())
    }
}

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
    use std::cmp::min;

    use super::{Duration, Rng};

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
    fn test_clock_advances() {
        let clock = TestClock::new();
        let start = clock.now_instant();

        clock.advance(Duration::from_secs(10));

        let elapsed = clock.now_instant().duration_since(start);
        assert_eq!(elapsed, Duration::from_secs(10));
    }

    #[test]
    fn test_clock_system_time() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let clock = TestClock::with_start_time(start);

        assert_eq!(clock.now_system(), start);

        clock.advance(Duration::from_secs(60));
        assert_eq!(clock.now_system(), start + Duration::from_secs(60));
    }

    #[test]
    fn test_clock_jump() {
        let clock = TestClock::new();
        let target = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);

        clock.jump_to(target);
        assert_eq!(clock.now_system(), target);
    }

    #[test]
    fn test_clock_advances_milliseconds() {
        let clock = TestClock::new();
        let start_time = clock.now_system();

        // Advance by 10 milliseconds
        clock.advance(Duration::from_millis(10));

        let end_time = clock.now_system();
        let elapsed = end_time.duration_since(start_time).unwrap();

        assert_eq!(elapsed, Duration::from_millis(10), "Clock should advance by exactly 10ms");

        // Test multiple small advances
        for _ in 0..5 {
            clock.advance(Duration::from_millis(5));
        }

        let final_time = clock.now_system();
        let total_elapsed = final_time.duration_since(start_time).unwrap();
        assert_eq!(
            total_elapsed,
            Duration::from_millis(35),
            "Multiple small advances should accumulate"
        );
    }

    #[tokio::test]
    async fn test_clock_sleep() {
        let clock = TestClock::new();
        let start = clock.now();

        clock.sleep(Duration::from_secs(5)).await;

        let elapsed = clock.now().duration_since(start);
        assert_eq!(elapsed, Duration::from_secs(5));
    }

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
}
