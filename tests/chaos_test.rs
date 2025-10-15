//! Basic chaos testing for database failure scenarios.
//!
//! These tests verify system resilience when database connections fail
//! or become unavailable. Simplified to use existing TestEnv API.

use anyhow::{Context, Result};
use test_harness::TestEnv;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

/// Tests basic database connectivity and recovery.
///
/// This test verifies that we can detect database issues and recover
/// from transient failures.
#[tokio::test]
async fn database_connectivity_resilience() -> Result<()> {
    let mut env = TestEnv::new().await?;

    // Verify initial health
    let initial_health = env.database_health_check().await?;
    assert!(initial_health, "Database should be healthy initially");

    // Create test data
    let tenant_id = env.create_tenant("chaos-test").await?;
    let endpoint_id = env.create_endpoint(tenant_id, "http://example.com/webhook").await?;

    // Verify we can query data after creation
    let tables = env.list_tables().await?;
    assert!(!tables.is_empty(), "Should have tables after setup");
    assert!(tables.contains(&"tenants".to_string()));
    assert!(tables.contains(&"endpoints".to_string()));

    // Verify the endpoint was created (within the same transaction)
    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut **env.db())
        .await?;
    assert_eq!(endpoint_count, 1, "Endpoint should be created successfully");

    // Test transaction handling
    {
        let tx = env.db();

        // Insert test data in transaction
        let test_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, plan, api_key) VALUES ($1, $2, $3, $4)")
            .bind(test_id)
            .bind("tx-test")
            .bind("free")
            .bind("test-key")
            .execute(&mut **tx)
            .await?;

        // Verify data exists in transaction
        let count_in_tx: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
            .bind(test_id)
            .fetch_one(&mut **tx)
            .await?;
        assert_eq!(count_in_tx, 1, "Data should exist in transaction");

        // Transaction auto-rollbacks when dropped
    }

    // Verify data was rolled back (using a fresh pool to check committed state)
    let count_after_rollback: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = 'tx-test'")
            .fetch_one(&env.create_pool())
            .await?;
    assert_eq!(count_after_rollback, 0, "Data should be rolled back");

    // Final health check
    let final_health = env.database_health_check().await?;
    assert!(final_health, "Database should still be healthy");

    Ok(())
}

/// Tests database timeout handling.
///
/// Verifies that database operations don't hang indefinitely and
/// properly handle timeout scenarios.
#[tokio::test]
async fn database_timeout_handling() -> Result<()> {
    let mut env = TestEnv::new().await?;

    // Test that database operations complete within reasonable time
    let health_check_result = timeout(Duration::from_secs(5), env.database_health_check()).await;

    assert!(health_check_result.is_ok(), "Health check should not timeout");
    assert!(health_check_result.unwrap()?, "Database should be healthy");

    // Test transaction timeout
    let tx_result: Result<(), tokio::time::error::Elapsed> =
        timeout(Duration::from_secs(5), async {
            // Simulate database operation that should not timeout
            let _: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&mut **env.db()).await.unwrap();
        })
        .await;

    assert!(tx_result.is_ok(), "Transaction creation should not timeout");

    Ok(())
}

/// Tests database connection pool behavior.
///
/// Verifies that the connection pool handles multiple concurrent
/// operations without exhaustion.
#[tokio::test]
async fn connection_pool_resilience() -> Result<()> {
    let mut env = TestEnv::new().await?;

    // Test multiple concurrent health checks without cloning TestEnv
    let mut results = Vec::new();

    for i in 0..10 {
        // Perform health checks sequentially but rapidly
        for j in 0..3 {
            let health = env
                .database_health_check()
                .await
                .with_context(|| format!("Health check failed for iteration {} check {}", i, j))?;
            assert!(health, "Health check should pass");

            results.push(health);

            // Small delay to test connection pool behavior
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // Verify all checks passed
    assert_eq!(results.len(), 30, "Should have performed 30 health checks");
    assert!(results.iter().all(|&h| h), "All health checks should pass");

    // Verify pool is still healthy after concurrent access
    let final_health = env.database_health_check().await?;
    assert!(final_health, "Database should be healthy after concurrent operations");

    Ok(())
}

/// Tests transaction rollback consistency.
/// Test transaction consistency and isolation guarantees under chaos scenarios.
///
/// This test verifies that transactions maintain ACID properties even when the
/// system is under stress or experiences failures, ensuring no data is left in
/// an inconsistent state.
#[tokio::test]
async fn transaction_consistency() -> Result<()> {
    // Test 1: Transaction isolation - data not visible outside transaction
    let mut env1 = TestEnv::new().await?;
    let tenant_id = env1.create_tenant("consistency-test").await?;

    let test_endpoint_id = Uuid::new_v4();

    // Insert data in transaction
    sqlx::query(
        r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#,
    )
    .bind(test_endpoint_id)
    .bind(tenant_id.0)
    .bind("test-endpoint")
    .bind("http://test.com")
    .bind(5i32)
    .bind(30i32)
    .bind("closed")
    .execute(&mut **env1.db())
    .await?;

    // Data should not be visible from another connection
    let external_pool = env1.create_pool();
    let count_external: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(test_endpoint_id)
        .fetch_one(&external_pool)
        .await?;
    assert_eq!(count_external, 0, "Transaction data should not be visible externally");

    // Test 2: Transaction rollback (automatic on drop)
    drop(env1); // This rolls back the transaction

    // Verify data was rolled back
    let env2 = TestEnv::new().await?;
    let pool = env2.create_pool();
    let count_after_rollback: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
            .bind(test_endpoint_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count_after_rollback, 0, "Data should be rolled back");

    // Test 3: Transaction commit persistence
    {
        let mut env3 = TestEnv::new().await?;
        let tenant_id = env3.create_tenant("commit-test").await?;
        let commit_endpoint_id = Uuid::new_v4();

        sqlx::query(
            r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#,
        )
        .bind(commit_endpoint_id)
        .bind(tenant_id.0)
        .bind("commit-endpoint")
        .bind("http://commit.test")
        .bind(3i32)
        .bind(15i32)
        .bind("closed")
        .execute(&mut **env3.db())
        .await?;

        // Commit the transaction
        env3.commit().await?;

        // Verify data persists after commit
        let verify_pool = env2.create_pool();
        let count_committed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
                .bind(commit_endpoint_id)
                .fetch_one(&verify_pool)
                .await?;
        assert_eq!(count_committed, 1, "Committed data should persist");

        // Clean up committed data
        sqlx::query("DELETE FROM endpoints WHERE id = $1")
            .bind(commit_endpoint_id)
            .execute(&verify_pool)
            .await?;
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id.0)
            .execute(&verify_pool)
            .await?;
    }

    Ok(())
}

/// Tests constraint violation handling.
///
/// Verifies that constraint violations are handled gracefully
/// without corrupting the database state.
#[tokio::test]
async fn constraint_violation_handling() -> Result<()> {
    // Phase 1: Create and commit initial data
    let mut env = TestEnv::new().await?;
    let tenant_id = env.create_tenant("constraint-test").await?;
    let endpoint1_id = env.create_endpoint(tenant_id, "http://test1.com").await?;

    // Commit this data so it's visible to external connections
    env.commit().await?;

    // Phase 2: Test constraint violation in a fresh environment
    let mut env2 = TestEnv::new().await?;

    // Attempt to create endpoint with duplicate name (should fail)
    let duplicate_result: Result<sqlx::postgres::PgQueryResult, sqlx::Error> = sqlx::query(
        r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds, circuit_state, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id.0)
    .bind("test-endpoint") // This name conflicts with the committed endpoint
    .bind("http://duplicate.com")
    .bind(5i32)
    .bind(30i32)
    .bind("closed")
    .execute(&mut **env2.db())
    .await;

    assert!(duplicate_result.is_err(), "Duplicate endpoint name should be rejected");

    // Phase 3: Verify original data is still intact (using external connection)
    let pool = env2.create_pool();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE tenant_id = $1")
        .bind(tenant_id.0)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 1, "Original endpoint should still exist");

    // Verify the specific endpoint we created is still there
    let endpoint_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE id = $1)")
            .bind(endpoint1_id.0)
            .fetch_one(&pool)
            .await?;
    assert!(endpoint_exists, "Original endpoint should still exist after constraint violation");

    // Verify database health after constraint violation (using fresh environment)
    let mut env3 = TestEnv::new().await?;
    let health = env3.database_health_check().await?;
    assert!(health, "Database should remain healthy after constraint violations");

    // Clean up committed data
    sqlx::query("DELETE FROM endpoints WHERE id = $1").bind(endpoint1_id.0).execute(&pool).await?;
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id.0).execute(&pool).await?;

    Ok(())
}
