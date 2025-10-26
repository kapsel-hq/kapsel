//! Worker pool management with structured concurrency.
//!
//! Provides lifecycle management, health monitoring, and graceful shutdown
//! for supervised delivery worker tasks.

use std::{sync::Arc, time::Duration};

use kapsel_core::{Clock, EventHandler, NoOpEventHandler};
use sqlx::PgPool;
use tokio::{sync::RwLock, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    circuit::CircuitBreakerManager,
    client::DeliveryClient,
    error::{DeliveryError, Result},
    worker::{DeliveryConfig, EngineStats},
};

/// Worker pool that manages delivery worker tasks with supervision.
///
/// The worker pool provides structured concurrency for webhook delivery
/// workers, ensuring proper lifecycle management, health monitoring, and
/// graceful shutdown. All workers are supervised and can be collectively
/// managed.
pub struct WorkerPool {
    pool: PgPool,
    config: DeliveryConfig,
    client: Arc<DeliveryClient>,
    circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
    stats: Arc<RwLock<EngineStats>>,
    cancellation_token: CancellationToken,
    worker_handles: Vec<JoinHandle<Result<()>>>,
    clock: Arc<dyn Clock>,
    event_handler: Arc<dyn EventHandler>,
}

impl WorkerPool {
    /// Create a new worker pool with the given configuration.
    pub fn new(
        pool: PgPool,
        config: DeliveryConfig,
        client: Arc<DeliveryClient>,
        circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
        stats: Arc<RwLock<EngineStats>>,
        cancellation_token: CancellationToken,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            worker_handles: Vec::new(),
            clock,
            event_handler: Arc::new(NoOpEventHandler),
        }
    }

    /// Create a new worker pool with event handler support.
    #[allow(clippy::too_many_arguments)]
    pub fn with_event_handler(
        pool: PgPool,
        config: DeliveryConfig,
        client: Arc<DeliveryClient>,
        circuit_manager: Arc<RwLock<CircuitBreakerManager>>,
        stats: Arc<RwLock<EngineStats>>,
        cancellation_token: CancellationToken,
        clock: Arc<dyn Clock>,
        event_handler: Arc<dyn EventHandler>,
    ) -> Self {
        Self {
            pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            worker_handles: Vec::new(),
            clock,
            event_handler,
        }
    }

    /// Spawn all configured workers and begin processing.
    ///
    /// Workers will run until cancellation is requested via the cancellation
    /// token. Returns immediately after spawning all workers.
    ///
    /// # Errors
    ///
    /// Currently never returns error but signature allows for future
    /// validation.
    pub async fn spawn_workers(&mut self) -> Result<()> {
        info!(worker_count = self.config.worker_count, "spawning delivery workers");

        {
            let mut stats = self.stats.write().await;
            stats.active_workers = self.config.worker_count;
        }

        for worker_id in 0..self.config.worker_count {
            let storage = Arc::new(kapsel_core::storage::Storage::new(self.pool.clone()));
            let worker = DeliveryWorker::new(
                worker_id,
                self.pool.clone(),
                storage,
                self.config.clone(),
                self.client.clone(),
                self.circuit_manager.clone(),
                self.stats.clone(),
                self.cancellation_token.clone(),
                self.event_handler.clone(),
                self.clock.clone(),
            );

            let handle = tokio::spawn(async move {
                info!(worker_id, "delivery worker starting");

                let result = worker.run().await;

                if let Err(ref error) = result {
                    error!(
                        worker_id,
                        error = %error,
                        "delivery worker terminated with error"
                    );
                } else {
                    info!(worker_id, "delivery worker stopped gracefully");
                }

                result
            });

            self.worker_handles.push(handle);
        }

        info!(
            spawned_workers = self.worker_handles.len(),
            "all delivery workers spawned successfully"
        );

        Ok(())
    }

    /// Gracefully shutdown all workers, waiting for in-flight deliveries to
    /// complete.
    ///
    /// This method will signal cancellation to all workers and wait for them to
    /// complete their current work within the configured shutdown timeout.
    ///
    /// # Errors
    ///
    /// Returns error if shutdown timeout is exceeded or workers fail to join.
    pub async fn shutdown_graceful(mut self, timeout: Duration) -> Result<()> {
        info!(
            worker_count = self.worker_handles.len(),
            timeout_seconds = timeout.as_secs(),
            "initiating graceful worker shutdown"
        );

        self.cancellation_token.cancel();

        let shutdown_future = async {
            let mut results = Vec::new();

            for (worker_id, handle) in
                std::mem::take(&mut self.worker_handles).into_iter().enumerate()
            {
                match handle.await {
                    Ok(worker_result) => {
                        if let Err(error) = worker_result {
                            warn!(
                                worker_id,
                                error = %error,
                                "worker completed with error during shutdown"
                            );
                        }
                        results.push(Ok(()));
                    },
                    Err(join_error) => {
                        error!(
                            worker_id,
                            error = %join_error,
                            "worker task panicked during shutdown"
                        );
                        results.push(Err(DeliveryError::WorkerPanic {
                            worker_id,
                            error: format!("{join_error}"),
                        }));
                    },
                }
            }

            {
                let mut stats = self.stats.write().await;
                stats.active_workers = 0;
            }

            results
        };

        match tokio::time::timeout(timeout, shutdown_future).await {
            Ok(results) => {
                let error_count = results.iter().filter(|r| r.is_err()).count();
                if error_count > 0 {
                    warn!(
                        error_count,
                        total_workers = results.len(),
                        "some workers completed with errors during shutdown"
                    );
                }
                info!("worker pool shutdown completed");
                Ok(())
            },
            Err(_timeout) => {
                error!(
                    timeout_seconds = timeout.as_secs(),
                    "worker shutdown timed out, some workers may still be running"
                );
                Err(DeliveryError::ShutdownTimeout { timeout })
            },
        }
    }

    /// Check if any workers are still running.
    pub fn has_active_workers(&self) -> bool {
        self.worker_handles.iter().any(|h| !h.is_finished())
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        if !self.worker_handles.is_empty() {
            let active_count = self.worker_handles.iter().filter(|h| !h.is_finished()).count();

            if active_count > 0 && !self.cancellation_token.is_cancelled() {
                error!(
                    active_workers = active_count,
                    "WorkerPool dropped with {} active workers! Forcing cancellation to prevent orphaned tasks",
                    active_count
                );

                self.cancellation_token.cancel();

                warn!(
                    "WorkerPool was not shut down gracefully. Call shutdown_graceful() before dropping to ensure clean shutdown."
                );
            }
        }
    }
}

use crate::worker::DeliveryWorker;
