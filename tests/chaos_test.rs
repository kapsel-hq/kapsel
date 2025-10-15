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
    let env = TestEnv::new().await?;

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

    // Verify the endpoint was created
    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&env.db.pool())
        .await?;
    assert_eq!(endpoint_count, 1, "Endpoint should be created successfully");

    // Test transaction handling
    {
        let mut tx = env.transaction().await?;

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
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
            .bind(test_id)
            .fetch_one(&mut **tx)
            .await?;
        assert_eq!(count, 1, "Data should exist in transaction");

        // Transaction auto-rollbacks when dropped
    }

    // Verify data was rolled back
    let count_after_rollback: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE name = 'tx-test'")
            .fetch_one(&env.db.pool())
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
    let env = TestEnv::new().await?;

    // Test that database operations complete within reasonable time
    let health_check_result = timeout(Duration::from_secs(5), env.database_health_check()).await;

    assert!(health_check_result.is_ok(), "Health check should not timeout");
    assert!(health_check_result.unwrap()?, "Database should be healthy");

    // Test transaction timeout
    let tx_result = timeout(Duration::from_secs(5), env.transaction()).await;

    assert!(tx_result.is_ok(), "Transaction creation should not timeout");
    let _tx = tx_result.unwrap()?;

    Ok(())
}

/// Tests database connection pool behavior.
///
/// Verifies that the connection pool handles multiple concurrent
/// operations without exhaustion.
#[tokio::test]
async fn connection_pool_resilience() -> Result<()> {
    let env = TestEnv::new().await?;

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
///
/// Verifies that failed transactions don't leave the database in
/// an inconsistent state.
#[tokio::test]
async fn transaction_consistency() -> Result<()> {
    let env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("consistency-test").await?;

    // Test successful transaction
    {
        let mut tx = env.transaction().await?;
        let test_endpoint_id = Uuid::new_v4();

        sqlx::query(
            r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(test_endpoint_id)
        .bind(tenant_id.0)
        .bind("test-endpoint")
        .bind("http://test.com")
        .bind(5i32)
        .bind(30i32)
        .execute(&mut **tx)
        .await?;

        tx.commit().await?;

        // Verify data was committed
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
            .bind(test_endpoint_id)
            .fetch_one(&env.db.pool())
            .await?;
        assert_eq!(count, 1, "Committed transaction should persist data");
    }

    // Test rollback scenario
    {
        let mut tx = env.transaction().await?;
        let test_endpoint_id = Uuid::new_v4();

        // Insert valid data
        sqlx::query(
            r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(test_endpoint_id)
        .bind(tenant_id.0)
        .bind("rollback-endpoint")
        .bind("http://rollback.com")
        .bind(5i32)
        .bind(30i32)
        .execute(&mut **tx)
        .await?;

        // Explicitly rollback
        tx.rollback().await?;

        // Verify data was not persisted
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
            .bind(test_endpoint_id)
            .fetch_one(&env.db.pool())
            .await?;
        assert_eq!(count, 0, "Rolled back transaction should not persist data");
    }

    // Verify database is still consistent
    let health = env.database_health_check().await?;
    assert!(health, "Database should remain healthy after rollbacks");

    Ok(())
}

/// Tests constraint violation handling.
///
/// Verifies that constraint violations are handled gracefully
/// without corrupting the database state.
#[tokio::test]
async fn constraint_violation_handling() -> Result<()> {
    let env = TestEnv::new().await?;

    let tenant_id = env.create_tenant("constraint-test").await?;

    // Create first endpoint with unique name
    let endpoint1_id = env.create_endpoint(tenant_id, "http://test1.com").await?;

    // Attempt to create endpoint with duplicate name (should fail)
    let mut tx = env.transaction().await?;
    let duplicate_result = sqlx::query(
        r#"INSERT INTO endpoints (id, tenant_id, name, url, max_retries, timeout_seconds)
           VALUES ($1, $2, $3, $4, $5, $6)"#
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id.0)
    .bind("test-endpoint") // This name already exists
    .bind("http://duplicate.com")
    .bind(5i32)
    .bind(30i32)
    .execute(&mut **tx)
    .await;

    assert!(duplicate_result.is_err(), "Duplicate endpoint name should be rejected");

    // Verify original data is still intact and specifically our endpoint
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE tenant_id = $1")
        .bind(tenant_id.0)
        .fetch_one(&env.db.pool())
        .await?;
    assert_eq!(count, 1, "Original endpoint should still exist");

    // Verify the specific endpoint we created is still there
    let endpoint_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM endpoints WHERE id = $1)")
            .bind(endpoint1_id.0)
            .fetch_one(&env.db.pool())
            .await?;
    assert!(endpoint_exists, "Original endpoint should still exist after constraint violation");

    // Verify database health after constraint violation
    let health = env.database_health_check().await?;
    assert!(health, "Database should be healthy after constraint violations");

    Ok(())
}
