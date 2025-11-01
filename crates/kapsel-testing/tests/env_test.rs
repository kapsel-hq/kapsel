//! Integration tests for TestEnv functionality.
//!
//! Tests the TestEnv API including database helpers, transaction management,
//! and utility methods. Uses shared database with transaction isolation.

use anyhow::Result;
use kapsel_testing::TestEnv;

#[tokio::test]
async fn test_env_pool_access_works() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    // Create data using transaction helper
    let tenant_id = env.create_tenant_tx(&mut tx, "pool-test-tenant").await?;

    // Verify we can query it back within the same transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1);

    // Transaction automatically rolls back when dropped
    Ok(())
}

#[tokio::test]
async fn test_env_transaction_methods_work() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Verify tenant exists within transaction
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(tenant_count, 1);

    // Verify endpoint exists within transaction
    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(endpoint_count, 1);

    // Transaction automatically rolls back when dropped
    Ok(())
}

#[tokio::test]
async fn health_check_works() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    assert!(env.database_health_check().await?);
    Ok(())
}

#[tokio::test]
async fn list_tables_returns_schema() -> Result<()> {
    let env = TestEnv::new_shared().await?;
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
    let env = TestEnv::new_shared().await?;
    env.verify_connection().await?;
    Ok(())
}

#[tokio::test]
async fn test_create_pool() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let pool2 = env.create_pool();

    // Verify the new pool works
    let result: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&pool2).await?;
    assert_eq!(result.0, 1);

    Ok(())
}

#[tokio::test]
async fn test_time_control() -> Result<()> {
    use std::time::Duration;

    let env = TestEnv::new_shared().await?;

    let start_time = env.now();
    env.advance_time(Duration::from_secs(60));
    let end_time = env.now();

    let elapsed = end_time - start_time;
    assert_eq!(elapsed, Duration::from_secs(60));

    Ok(())
}

#[tokio::test]
async fn test_api_key_creation() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "key-test-tenant").await?;
    let (api_key, key_hash) = env.create_api_key_tx(&mut tx, tenant_id, "test-key").await?;

    // Verify the API key was created within transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE key_hash = $1")
        .bind(&key_hash)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1);

    // Verify key properties
    assert!(!api_key.is_empty());
    assert!(!key_hash.is_empty());
    assert_ne!(api_key, key_hash);

    // Transaction automatically rolls back when dropped
    Ok(())
}

#[tokio::test]
async fn test_tenant_with_plan() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id =
        env.create_tenant_with_plan_tx(&mut tx, "enterprise-tenant", "enterprise").await?;

    // Verify the tier was set correctly within transaction
    let tier = env.get_tenant_tier_tx(&mut tx, tenant_id).await?;
    assert_eq!(tier, "enterprise");

    // Transaction automatically rolls back when dropped
    Ok(())
}

#[tokio::test]
async fn test_endpoint_with_config() -> Result<()> {
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "config-test-tenant").await?;
    let endpoint_id = env
        .create_endpoint_with_config_tx(
            &mut tx,
            tenant_id,
            "https://example.com/hook",
            "configured-endpoint",
            5,
            30,
        )
        .await?;

    // Verify endpoint was created with correct config within transaction
    let (url, name, max_retries, timeout): (String, String, i32, i32) = sqlx::query_as(
        "SELECT url, name, max_retries, timeout_seconds FROM endpoints WHERE id = $1",
    )
    .bind(endpoint_id.0)
    .fetch_one(&mut *tx)
    .await?;

    assert_eq!(url, "https://example.com/hook");
    assert!(name.contains("configured-endpoint"));
    assert_eq!(max_retries, 5);
    assert_eq!(timeout, 30);

    // Transaction automatically rolls back when dropped
    Ok(())
}

#[tokio::test]
async fn test_debug_helpers() -> Result<()> {
    let env = TestEnv::new_shared().await?;

    // Test pool stats
    let stats = env.debug_pool_stats();
    assert!(stats.contains("size="));
    assert!(stats.contains("num_idle="));

    // Test event listing (may contain events from other tests in shared DB)
    let _events = env.debug_list_events().await?;
    // Just verify the method works (events.len() is always >= 0 by definition)

    Ok(())
}

#[tokio::test]
async fn test_count_methods() -> Result<()> {
    let env = TestEnv::new_shared().await?;

    // In shared database, counts may not be zero due to other tests
    // Just verify methods work and return reasonable values
    let total = env.count_total_events().await?;
    let terminal = env.count_terminal_events().await?;
    let processing = env.count_processing_events().await?;
    let pending = env.count_pending_events().await?;

    // Verify counts are non-negative
    assert!(total >= 0);
    assert!(terminal >= 0);
    assert!(processing >= 0);
    assert!(pending >= 0);

    Ok(())
}

#[tokio::test]
async fn test_isolated_vs_shared() -> Result<()> {
    // Test that both initialization methods work
    let shared_env = TestEnv::new_shared().await?;
    let isolated_env = TestEnv::new_isolated().await?;

    // Both should have working pool access
    let _pool1 = shared_env.pool();
    let _pool2 = isolated_env.pool();

    // Verify isolation flag is set correctly
    assert!(!shared_env.is_isolated_test());
    assert!(isolated_env.is_isolated_test());

    Ok(())
}

#[tokio::test]
async fn test_repository_access_with_committed_data() -> Result<()> {
    // This test demonstrates how to test repository methods within a transaction
    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "repo-test-tenant").await?;

    // Test repository access within the transaction using _in_tx methods
    let tenant = env.storage().tenants.find_by_id_in_tx(&mut tx, tenant_id).await?;
    assert!(tenant.is_some(), "Expected tenant to exist in repository");

    let found_tenant = tenant.unwrap();
    assert_eq!(found_tenant.id, tenant_id);
    assert!(found_tenant.name.contains("repo-test-tenant"));

    // Transaction automatically rolls back when dropped - no cleanup needed

    Ok(())
}

#[tokio::test]
async fn test_circuit_breaker_helpers() -> Result<()> {
    use kapsel_core::models::CircuitState;

    let env = TestEnv::new_shared().await?;
    let mut tx = env.pool().begin().await?;

    let tenant_id = env.create_tenant_tx(&mut tx, "circuit-test-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Test circuit breaker helper methods
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Open, 5, 0).await?;

    // Verify within transaction
    let endpoint = env.storage().endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await?;
    assert!(endpoint.is_some());
    assert_eq!(endpoint.unwrap().circuit_state, CircuitState::Open);

    // Reset to closed
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Closed, 0, 0).await?;

    let endpoint = env.storage().endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await?;
    assert_eq!(endpoint.unwrap().circuit_state, CircuitState::Closed);

    // Transaction automatically rolls back when dropped
    Ok(())
}
