//! Time abstractions for testable and configurable timing operations.
//!
//! Provides clock abstraction to enable deterministic testing and flexible
//! time control in webhook delivery systems.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Clock abstraction for time operations.
///
/// Enables dependency injection of time sources for testing and flexibility.
/// Production code uses `RealClock`, tests can inject controllable
/// implementations.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Returns the current instant for duration measurements.
    fn now(&self) -> Instant;

    /// Returns the current system time for timestamps.
    fn now_system(&self) -> SystemTime;

    /// Sleeps for the specified duration.
    ///
    /// In production this maps to tokio::time::sleep, in tests this can
    /// advance virtual time immediately.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Real clock implementation using system time.
///
/// This is the production implementation that uses actual system time
/// and tokio's async sleep mechanism.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn now_system(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

impl RealClock {
    /// Creates a new real clock instance.
    pub fn new() -> Self {
        Self
    }
}

/// Test clock for deterministic time control.
///
/// Provides controllable time progression for reproducible testing of
/// time-dependent functionality. Both monotonic and system time can be
/// controlled independently.
#[derive(Debug, Clone)]
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
        let elapsed_ns = self.monotonic_ns.load(Ordering::Acquire);
        self.base_instant + Duration::from_nanos(elapsed_ns)
    }

    fn now_system(&self) -> SystemTime {
        let ns = self.system_ns.load(Ordering::Acquire);
        UNIX_EPOCH + Duration::from_nanos(ns)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        // In tests, sleep just advances the clock
        self.advance(duration);
        // Yield to allow other tasks to run
        Box::pin(tokio::task::yield_now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_advances() {
        let clock = TestClock::new();
        let start = clock.now();

        clock.advance(Duration::from_secs(10));

        let elapsed = clock.now().duration_since(start);
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

    #[tokio::test]
    async fn test_clock_sleep() {
        let clock = TestClock::new();
        let start = clock.now();

        clock.sleep(Duration::from_secs(5)).await;

        let elapsed = clock.now().duration_since(start);
        assert_eq!(elapsed, Duration::from_secs(5));
    }
}
