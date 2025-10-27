//! Integration tests for production delivery engine in test harness.

use std::time::Duration;

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::TestEnv;

#[tokio::test]
async fn production_engine_can_be_created() -> Result<()> {
    let env = TestEnv::builder().worker_count(1).batch_size(5).isolated().build().await?;

    // Verify engine was created and has initial stats
    let stats = env.delivery_stats().await;
    assert!(stats.is_some(), "delivery engine should be configured");

    let stats = stats.unwrap();
    assert_eq!(stats.active_workers, 0); // Workers not started yet
    assert_eq!(stats.successful_deliveries, 0);
    assert_eq!(stats.events_processed, 0);

    Ok(())
}

#[tokio::test]
async fn production_engine_basic_integration() -> Result<()> {
    let env = TestEnv::builder().worker_count(1).isolated().build().await?;

    // Create test data
    let tenant = env.create_tenant("integration-tenant").await?;
    let endpoint_id = env.create_endpoint(tenant, "https://example.com/test").await?;

    // Ingest a webhook
    let event_id = env.ingest_webhook_simple(endpoint_id, b"test payload").await?;

    // Verify webhook was created in pending state
    let status = env.event_status(event_id).await?;
    assert_eq!(status, EventStatus::Pending);

    Ok(())
}

#[tokio::test]
async fn test_clock_integration() -> Result<()> {
    let env = TestEnv::builder().isolated().build().await?;

    let start_time = env.now();

    // Advance test clock
    env.advance_time(Duration::from_secs(5));

    let after_advance = env.now();
    assert!(after_advance > start_time);

    Ok(())
}

#[tokio::test]
async fn builder_configures_engine_correctly() -> Result<()> {
    let env = TestEnv::builder()
        .worker_count(3)
        .batch_size(20)
        .poll_interval(Duration::from_millis(200))
        .shutdown_timeout(Duration::from_secs(10))
        .isolated()
        .build()
        .await?;

    // Verify engine was created
    assert!(env.delivery_stats().await.is_some());

    Ok(())
}

#[tokio::test]
async fn test_env_without_engine() -> Result<()> {
    let env = TestEnv::builder().without_delivery_engine().isolated().build().await?;

    // Should have no delivery stats when engine disabled
    assert!(env.delivery_stats().await.is_none());

    Ok(())
}

#[tokio::test]
async fn database_operations_work() -> Result<()> {
    let env = TestEnv::builder().isolated().build().await?;

    let tenant = env.create_tenant("db-test-tenant").await?;
    let endpoint_id = env.create_endpoint(tenant, "https://example.com/test").await?;

    // Create multiple webhooks
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let payload = format!("payload-{}", i);
        event_ids.push(env.ingest_webhook_simple(endpoint_id, payload.as_bytes()).await?);
    }

    // Verify all were created
    for event_id in event_ids {
        let status = env.event_status(event_id).await?;
        assert_eq!(status, EventStatus::Pending);
    }

    Ok(())
}

#[tokio::test]
async fn storage_integration_works() -> Result<()> {
    let env = TestEnv::builder().isolated().build().await?;

    let tenant = env.create_tenant("storage-tenant").await?;

    // Verify we can access storage through TestEnv
    let storage = env.storage();
    let found_tenant = storage.tenants.find_by_id(tenant).await?;

    assert!(found_tenant.is_some());
    assert_eq!(found_tenant.unwrap().id, tenant);

    Ok(())
}
