//! Time abstractions for testable and configurable timing operations.
//!
//! Provides clock abstraction to enable deterministic testing and flexible
//! time control in webhook delivery systems.

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant, SystemTime},
};

/// Clock abstraction for time operations.
///
/// Enables dependency injection of time sources for testing and flexibility.
/// Production code uses `RealClock`, tests can inject controllable
/// implementations.
pub trait Clock: Send + Sync {
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
