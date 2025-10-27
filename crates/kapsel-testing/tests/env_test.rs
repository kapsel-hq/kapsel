//! Tests for TestEnv core functionality.

use anyhow::Result;
use kapsel_testing::TestEnv;
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
async fn test_env_pool_access_works() -> Result<()> {
    // Test that pool access works for persistent operations
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Create data using transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "pool-test-tenant").await?;
    tx.commit().await?;

    // Verify we can query it back by ID (SQL check)
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(count, 1);

    // Also verify using repository (tests both SQL and repository layer)
    let tenant = env.storage().tenants.find_by_id(tenant_id).await?;
    assert!(tenant.is_some(), "Expected tenant to exist in repository");
    // Verify the tenant exists and has the correct ID
    let found_id: Uuid = sqlx::query_scalar("SELECT id FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(found_id, tenant_id.0);

    Ok(())
}

#[tokio::test]
async fn test_env_transaction_methods_work() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;
    tx.commit().await?;

    // Verify tenant exists
    let tenant_count = env.count_by_id("tenants", "id", tenant_id.0).await?;
    assert_eq!(tenant_count, 1);

    // Verify endpoint exists
    let endpoint_count = env.count_by_id("endpoints", "id", endpoint_id.0).await?;
    assert_eq!(endpoint_count, 1);

    Ok(())
}

#[tokio::test]
async fn health_check_works() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    assert!(env.database_health_check().await?);
    Ok(())
}

#[tokio::test]
async fn list_tables_returns_schema() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let tables = env.list_tables().await?;

    // Verify key tables exist
    assert!(tables.contains(&"tenants".to_string()));
    assert!(tables.contains(&"endpoints".to_string()));
    assert!(tables.contains(&"webhook_events".to_string()));
    assert!(tables.contains(&"delivery_attempts".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_verify_connection() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    env.verify_connection().await?;
    Ok(())
}

#[tokio::test]
async fn test_create_pool() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let pool2 = env.create_pool();

    // Verify the new pool works
    let result: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&pool2).await?;
    assert_eq!(result.0, 1);

    Ok(())
}

#[tokio::test]
async fn test_time_control() -> Result<()> {
    use std::time::Duration;

    let env = TestEnv::new().await?;

    let start_time = env.now();
    env.advance_time(Duration::from_secs(60));
    let end_time = env.now();

    let elapsed = end_time - start_time;
    assert_eq!(elapsed, Duration::from_secs(60));

    Ok(())
}

#[tokio::test]
async fn test_api_key_creation() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "key-test-tenant").await?;
    let (api_key, key_hash) = env.create_api_key_tx(&mut tx, tenant_id, "test-key").await?;

    // Verify the API key was created with SQL count check
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE key_hash = $1")
        .bind(&key_hash)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1);

    tx.commit().await?;

    // Also verify using repository (tests both SQL and repository layer)
    let storage = kapsel_core::storage::Storage::new(env.pool().clone());
    let found_api_key = storage.api_keys.find_by_hash(&key_hash).await?;
    assert!(found_api_key.is_some(), "Expected API key to exist in repository");

    // Verify the key hash matches and tenant is correct
    let found_key = found_api_key.unwrap();
    assert_eq!(found_key.key_hash, key_hash);
    assert_eq!(found_key.tenant_id, tenant_id);

    // Verify the key hash matches
    assert_eq!(key_hash, sha256::digest(api_key.as_bytes()));
    Ok(())
}

#[tokio::test]
async fn test_tenant_with_plan() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id =
        env.create_tenant_with_plan_tx(&mut tx, "enterprise-tenant", "enterprise").await?;

    // Verify the tier was set correctly
    let tier: String = sqlx::query_scalar("SELECT tier FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(tier, "enterprise");

    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn test_endpoint_with_config() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "config-test-tenant").await?;
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            "https://example.com/hook",
            "configured-endpoint",
            5,  // max_retries
            60, // timeout_seconds
        )
        .await?;

    // Verify the configuration was set correctly
    let row = sqlx::query("SELECT max_retries, timeout_seconds FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut *tx)
        .await?;

    let max_retries: i32 = row.get("max_retries");
    let timeout_seconds: i32 = row.get("timeout_seconds");

    assert_eq!(max_retries, 5);
    assert_eq!(timeout_seconds, 60);

    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn test_debug_helpers() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // Test pool stats
    let stats = env.debug_pool_stats().await;
    assert!(stats.contains("size="));
    assert!(stats.contains("num_idle="));

    // Test event listing (should be empty initially)
    let events = env.debug_list_events().await?;
    assert_eq!(events.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_count_methods() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // All counts should be zero initially
    assert_eq!(env.count_total_events().await?, 0);
    assert_eq!(env.count_terminal_events().await?, 0);
    assert_eq!(env.count_processing_events().await?, 0);
    assert_eq!(env.count_pending_events().await?, 0);

    Ok(())
}

#[tokio::test]
async fn test_isolated_vs_shared() -> Result<()> {
    // Test that both initialization methods work
    let shared_env = TestEnv::new().await?;
    let isolated_env = TestEnv::new_isolated().await?;

    // Both should have working database connections
    assert!(shared_env.database_health_check().await?);
    assert!(isolated_env.database_health_check().await?);

    // Verify isolation flag is set correctly
    assert!(!shared_env.is_isolated_test());
    assert!(isolated_env.is_isolated_test());

    Ok(())
}
