//! Integration tests for worker pool cleanup and task management.
//!
//! Tests worker pool lifecycle, background task cleanup, and prevention
//! of orphaned workers after test completion.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use kapsel_delivery::{
    circuit::{CircuitBreakerManager, CircuitConfig},
    client::{ClientConfig, DeliveryClient},
    retry::RetryPolicy,
    worker::{DeliveryConfig, EngineStats},
    worker_pool::WorkerPool,
};
use kapsel_testing::TestEnv;
use tokio::{sync::RwLock, time::timeout};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn worker_pool_explicit_shutdown() -> Result<()> {
    let env = TestEnv::new().await?;

    let config = DeliveryConfig {
        worker_count: 2,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig { timeout: Duration::from_secs(5), ..Default::default() },
        default_retry_policy: RetryPolicy::default(),

        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let circuit_manager =
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
    let stats = Arc::new(RwLock::new(EngineStats::default()));
    let cancellation_token = CancellationToken::new();

    let mut pool = WorkerPool::new(
        env.create_pool(),
        config,
        client,
        circuit_manager,
        stats,
        cancellation_token,
    );

    pool.spawn_workers().await?;
    assert!(pool.has_active_workers(), "Workers should be active after spawning");

    // Let workers run briefly to ensure they're actually working
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Explicitly shut down workers (consumes pool, guaranteeing cleanup)
    pool.shutdown_graceful(Duration::from_secs(5)).await?;

    Ok(())
}

#[tokio::test]
async fn worker_pool_drop_cleanup() -> Result<()> {
    let env = TestEnv::new().await?;

    let config = DeliveryConfig {
        worker_count: 2,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig::default(),
        default_retry_policy: RetryPolicy::default(),

        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let circuit_manager =
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default())));
    let stats = Arc::new(RwLock::new(EngineStats::default()));
    let cancellation_token = CancellationToken::new();
    let cancellation_token_clone = cancellation_token.clone();

    {
        let mut pool = WorkerPool::new(
            env.create_pool(),
            config,
            client,
            circuit_manager,
            stats,
            cancellation_token,
        );

        pool.spawn_workers().await?;
        assert!(pool.has_active_workers(), "Workers should be active after spawning");

        // Let workers run briefly
        tokio::time::sleep(Duration::from_millis(50)).await;
    } // WorkerPool goes out of scope here, Drop should be called

    // Give the Drop implementation time to cancel workers
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify cancellation token was triggered by Drop
    assert!(cancellation_token_clone.is_cancelled(), "Drop should have cancelled the token");

    Ok(())
}

#[tokio::test]
async fn multiple_worker_pools_isolation() -> Result<()> {
    let env1 = TestEnv::new().await?;
    let env2 = TestEnv::new().await?;

    let config = DeliveryConfig {
        worker_count: 1,
        batch_size: 5,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig::default(),
        default_retry_policy: RetryPolicy::default(),

        shutdown_timeout: Duration::from_secs(5),
    };

    let client1 = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let mut pool1 = WorkerPool::new(
        env1.create_pool(),
        config.clone(),
        client1,
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default()))),
        Arc::new(RwLock::new(EngineStats::default())),
        CancellationToken::new(),
    );

    let client2 = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let mut pool2 = WorkerPool::new(
        env2.create_pool(),
        config,
        client2,
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default()))),
        Arc::new(RwLock::new(EngineStats::default())),
        CancellationToken::new(),
    );

    pool1.spawn_workers().await?;
    pool2.spawn_workers().await?;

    assert!(pool1.has_active_workers(), "Pool 1 should have active workers");
    assert!(pool2.has_active_workers(), "Pool 2 should have active workers");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify pool2 is still active before shutting down pool1
    assert!(pool2.has_active_workers(), "Pool 2 should still be running");

    // Shutdown pool1 (consumes pool1)
    pool1.shutdown_graceful(Duration::from_secs(5)).await?;

    // Pool2 should still be running
    assert!(pool2.has_active_workers(), "Pool 2 should still be running after pool1 shutdown");

    // Shutdown pool2 (consumes pool2)
    pool2.shutdown_graceful(Duration::from_secs(5)).await?;

    Ok(())
}

#[tokio::test]
async fn worker_pool_shutdown_timeout() -> Result<()> {
    let env = TestEnv::new().await?;

    let config = DeliveryConfig {
        worker_count: 1,
        batch_size: 10,
        poll_interval: Duration::from_millis(100),
        client_config: ClientConfig::default(),
        default_retry_policy: RetryPolicy::default(),

        shutdown_timeout: Duration::from_secs(5),
    };

    let client = Arc::new(DeliveryClient::new(config.client_config.clone())?);
    let mut pool = WorkerPool::new(
        env.create_pool(),
        config,
        client,
        Arc::new(RwLock::new(CircuitBreakerManager::new(CircuitConfig::default()))),
        Arc::new(RwLock::new(EngineStats::default())),
        CancellationToken::new(),
    );

    pool.spawn_workers().await?;

    // Shutdown should complete within reasonable time
    let shutdown_result = timeout(
        Duration::from_secs(10), // Generous timeout for the test itself
        pool.shutdown_graceful(Duration::from_millis(100)), // Short timeout for workers
    )
    .await;

    match shutdown_result {
        Ok(Ok(())) => {
            // Successful shutdown
        },
        Ok(Err(e)) => {
            // Worker shutdown may have timed out, which is acceptable for this test
            tracing::info!("Worker shutdown timed out as expected: {}", e);
        },
        Err(e) => {
            unreachable!("Test itself timed out - shutdown_graceful took too long: {}", e);
        },
    }

    Ok(())
}
