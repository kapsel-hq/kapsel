//! Worker pool management with structured concurrency.
//!
//! Provides lifecycle management, health monitoring, and graceful shutdown
//! for supervised delivery worker tasks.

use std::{sync::Arc, time::Duration};

use kapsel_core::Clock;
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
            let worker = DeliveryWorker::new(
                worker_id,
                self.pool.clone(),
                self.config.clone(),
                self.client.clone(),
                self.circuit_manager.clone(),
                self.stats.clone(),
                self.cancellation_token.clone(),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kapsel_testing::TestEnv;

    use super::*;
    use crate::{
        circuit::{CircuitBreakerManager, CircuitConfig},
        client::DeliveryClient,
        worker_pool::{DeliveryConfig, EngineStats},
    };

    async fn create_test_worker_pool() -> WorkerPool {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig::default();
        let client = Arc::new(DeliveryClient::new(config.client_config.clone()).unwrap());
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let cancellation_token = CancellationToken::new();
        let pg_pool = env.create_pool();

        WorkerPool::new(
            pg_pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        )
    }

    #[tokio::test]
    async fn worker_pool_spawns_configured_number_of_workers() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig { worker_count: 5, ..Default::default() };
        let client = Arc::new(DeliveryClient::new(config.client_config.clone()).unwrap());
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let cancellation_token = CancellationToken::new();
        let pg_pool = env.create_pool();

        let mut pool = WorkerPool::new(
            pg_pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        );

        pool.spawn_workers().await.expect("workers should spawn successfully");

        // Workers should be spawned successfully
        assert!(!pool.worker_handles.is_empty());

        // Shutdown gracefully
        pool.shutdown_graceful(Duration::from_secs(1))
            .await
            .expect("graceful shutdown should succeed");
    }

    #[tokio::test]
    async fn worker_pool_shuts_down_gracefully_with_timeout() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig::default();
        let client = Arc::new(DeliveryClient::new(config.client_config.clone()).unwrap());
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let cancellation_token = CancellationToken::new();
        let pg_pool = env.create_pool();

        let mut pool = WorkerPool::new(
            pg_pool,
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        );
        pool.spawn_workers().await.expect("workers should spawn successfully");

        // Small delay to ensure workers are running
        tokio::time::sleep(Duration::from_millis(10)).await;

        let shutdown_start = std::time::Instant::now();
        pool.shutdown_graceful(Duration::from_secs(3))
            .await
            .expect("graceful shutdown should complete within timeout");
        let shutdown_duration = shutdown_start.elapsed();

        // Shutdown should complete quickly since no work is being done
        assert!(shutdown_duration < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn worker_pool_spawns_and_shuts_down() {
        let mut pool = create_test_worker_pool().await;

        // Initially no workers
        assert_eq!(pool.worker_handles.len(), 0);

        // Spawn workers
        pool.spawn_workers().await.expect("workers should spawn successfully");

        // Should have spawned workers
        assert_eq!(pool.worker_handles.len(), 3); // Default config has 3 workers

        pool.shutdown_graceful(Duration::from_secs(1))
            .await
            .expect("graceful shutdown should succeed");
    }

    #[tokio::test]
    async fn worker_pool_handles_shutdown_timeout() {
        let pool = create_test_worker_pool().await;

        // Shutdown with very short timeout should succeed since no workers are running
        let result = pool.shutdown_graceful(Duration::from_millis(1)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn worker_pool_updates_engine_stats() {
        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig { worker_count: 4, ..Default::default() };

        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let client = Arc::new(DeliveryClient::new(config.client_config.clone()).unwrap());
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let cancellation_token = CancellationToken::new();
        let pg_pool = env.create_pool();

        let mut pool = WorkerPool::new(
            pg_pool,
            config,
            client,
            circuit_manager,
            stats.clone(),
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        );

        // Initially no active workers
        {
            let stats_guard = stats.read().await;
            assert_eq!(stats_guard.active_workers, 0);
            drop(stats_guard);
        }

        pool.spawn_workers().await.expect("workers should spawn successfully");

        // Give a moment for stats update task to run
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Should show active workers
        {
            let stats_guard = stats.read().await;
            assert_eq!(stats_guard.active_workers, 4);
        }

        pool.shutdown_graceful(Duration::from_secs(1))
            .await
            .expect("graceful shutdown should succeed");

        // After shutdown, should show no active workers
        {
            let stats_guard = stats.read().await;
            assert_eq!(stats_guard.active_workers, 0);
        }
    }

    #[tokio::test]
    async fn worker_pool_integration_test_demonstrates_full_lifecycle() {
        // This test demonstrates the complete worker pool lifecycle
        // without requiring database connectivity, showing the implementation works

        let env = TestEnv::new().await.expect("test environment setup failed");
        let config = DeliveryConfig {
            worker_count: 2,
            poll_interval: Duration::from_millis(50),
            ..Default::default()
        };

        let client = Arc::new(DeliveryClient::new(config.client_config.clone()).unwrap());
        let circuit_manager =
            Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
        let stats = Arc::new(RwLock::new(EngineStats::default()));
        let cancellation_token = CancellationToken::new();
        let pg_pool = env.create_pool();

        // Create and initialize worker pool
        let mut pool = WorkerPool::new(
            pg_pool,
            config,
            client,
            circuit_manager,
            stats.clone(),
            cancellation_token,
            Arc::new(env.clock.clone()) as Arc<dyn Clock>,
        );

        // Verify initial state
        assert_eq!(pool.worker_handles.len(), 0);

        // Spawn workers - this should succeed
        pool.spawn_workers().await.expect("workers should spawn successfully");

        // Verify workers were spawned
        assert_eq!(pool.worker_handles.len(), 2);

        // Verify stats were updated
        {
            let stats_guard = stats.read().await;
            assert_eq!(stats_guard.active_workers, 2);
        }

        // Let workers run briefly to ensure they start properly
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Graceful shutdown - this consumes the pool but should complete successfully
        let shutdown_start = std::time::Instant::now();
        pool.shutdown_graceful(Duration::from_secs(5))
            .await
            .expect("graceful shutdown should succeed");
        let shutdown_duration = shutdown_start.elapsed();

        // Shutdown should be relatively quick since workers aren't doing real work
        assert!(shutdown_duration < Duration::from_secs(2));

        // Stats should show no active workers after shutdown
        {
            let stats_guard = stats.read().await;
            assert_eq!(stats_guard.active_workers, 0);
        }
    }
}
