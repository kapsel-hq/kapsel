//! Production webhook delivery integration for TestEnv.
//!
//! This module provides integration with the actual production DeliveryEngine,
//! ensuring tests exercise the same code paths that run in production.
//! All simulation logic has been removed to prevent test/production divergence.

use std::time::Duration;

use anyhow::{Context, Result};
use kapsel_core::Clock;

use crate::TestEnv;

impl TestEnv {
    /// Processes pending webhooks using the production delivery engine.
    ///
    /// Starts the delivery engine workers, allows them to process pending
    /// webhooks, then gracefully shuts them down. This ensures tests use
    /// the exact production code paths for webhook delivery.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - No delivery engine is configured for this TestEnv
    /// - The delivery engine fails to start, process events, or shutdown
    /// - Database connectivity issues occur during processing
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub async fn run_delivery_cycle(&mut self) -> Result<()> {
        self.process_batch().await.context("delivery cycle failed")
    }

    /// Processes a single batch of pending webhooks using production engine.
    ///
    /// This method starts delivery workers and triggers batch processing.
    /// The engine is reused across multiple `process_batch()` calls to preserve
    /// statistics and avoid expensive worker thread recreation.
    ///
    /// # Errors
    ///
    /// Returns error if the delivery engine is not configured or fails to
    /// start.
    pub async fn process_batch(&self) -> Result<()> {
        if let Some(ref engine) = self.delivery_engine {
            let processed_count =
                engine.process_batch().await.context("failed to process batch")?;
            tracing::debug!(processed_count, "batch processing completed");
            Ok(())
        } else {
            anyhow::bail!(
                "No delivery engine configured. Use TestEnv::builder() to enable production engine."
            )
        }
    }

    /// Returns statistics from the production delivery engine.
    ///
    /// Returns None if no delivery engine is configured for this TestEnv.
    pub async fn get_delivery_stats(&self) -> Option<kapsel_delivery::worker::EngineStats> {
        if let Some(ref engine) = self.delivery_engine {
            Some(engine.stats().await)
        } else {
            None
        }
    }

    /// Alias for get_delivery_stats for backward compatibility.
    pub async fn delivery_stats(&self) -> Option<kapsel_delivery::worker::EngineStats> {
        self.get_delivery_stats().await
    }

    /// Waits for the delivery engine to process all pending events.
    ///
    /// This is a convenience method that repeatedly calls `process_batch()`
    /// until no more events are pending. Useful for tests that generate
    /// multiple events and want to ensure all are processed.
    ///
    /// # Errors
    ///
    /// Returns error if batch processing fails or timeout is exceeded.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub async fn process_all_pending(&mut self, timeout: Duration) -> Result<()> {
        const MAX_CYCLES: u32 = 50; // Prevent infinite loops

        let start_time = self.clock.now();
        let mut cycles = 0;

        loop {
            if start_time.elapsed() > timeout {
                anyhow::bail!("timeout processing all pending events after {cycles} cycles");
            }

            if cycles >= MAX_CYCLES {
                anyhow::bail!("exceeded maximum processing cycles ({MAX_CYCLES})");
            }

            let Some(stats_before) = self.get_delivery_stats().await else {
                anyhow::bail!("no delivery engine available")
            };

            // Process one batch
            self.process_batch().await?;
            cycles += 1;

            // Check if any new events were processed
            let Some(stats_after) = self.get_delivery_stats().await else {
                anyhow::bail!("no delivery engine available")
            };

            if stats_after.events_processed == stats_before.events_processed {
                // No new events processed, we're done
                tracing::debug!(
                    cycles,
                    total_processed = stats_after.events_processed,
                    "finished processing all pending events"
                );
                break;
            }

            // Brief pause between cycles
            self.clock.sleep(Duration::from_millis(10)).await;
        }

        Ok(())
    }

    /// Forces immediate shutdown of the delivery engine.
    ///
    /// This must be called before dropping TestEnv to ensure clean shutdown
    /// of worker threads and database connections. After calling this method,
    /// the delivery engine cannot be used again.
    pub async fn force_shutdown_delivery_engine(&mut self) -> Result<()> {
        if let Some(engine) = self.delivery_engine.take() {
            engine.shutdown().await.context("failed to shutdown delivery engine")?;
        }
        Ok(())
    }

    /// Consumes self and shuts down delivery engine.
    pub async fn shutdown_delivery_engine(mut self) -> Result<()> {
        self.force_shutdown_delivery_engine().await
    }
}
