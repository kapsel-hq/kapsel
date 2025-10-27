//! Tests for transaction isolation in TestEnv.

use anyhow::Result;
use kapsel_testing::TestEnv;
use uuid::Uuid;

#[tokio::test]
async fn test_transactions_provide_isolation() -> Result<()> {
    // Create two test environments
    let env1 = TestEnv::new_isolated().await?;
    let env2 = TestEnv::new_isolated().await?;

    // Each begins its own transaction
    let mut tx1 = env1.pool().begin().await?;
    let mut tx2 = env2.pool().begin().await?;

    // tx1 inserts data
    let tenant_id1 = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
        .bind(tenant_id1)
        .bind("tenant-tx1")
        .bind("free")
        .execute(&mut *tx1)
        .await?;

    // tx2 should NOT see tx1's data
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id1)
        .fetch_one(&mut *tx2)
        .await?;
    assert_eq!(count, 0, "tx2 should not see uncommitted data from tx1");

    // Explicit rollback to prevent connection leaks
    tx1.rollback().await?;
    tx2.rollback().await?;

    Ok(())
}

#[tokio::test]
async fn test_rollback_prevents_data_persistence() -> Result<()> {
    let tenant_id = {
        let env = TestEnv::new_isolated().await?;
        let mut tx = env.pool().begin().await?;

        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, plan) VALUES ($1, $2, $3)")
            .bind(id)
            .bind("rollback-test")
            .bind("free")
            .execute(&mut *tx)
            .await?;

        tx.rollback().await?;
        id
    };

    // Different environment should not see the rolled-back data
    let env = TestEnv::new_isolated().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(env.pool())
        .await?;

    assert_eq!(count, 0, "rolled back data should not persist");

    Ok(())
}

#[tokio::test]
async fn test_helper_methods_work_with_transactions() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Test that we can create tenant within a transaction
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, plan, created_at, updated_at) VALUES ($1, $2, $3, NOW(), NOW())")
        .bind(tenant_id)
        .bind("tx-tenant")
        .bind("free")
        .execute(&mut *tx)
        .await?;

    // Verify within same transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1);

    // Explicit rollback to prevent connection leak
    tx.rollback().await?;

    // Verify data was rolled back
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(env.pool())
        .await?;

    assert_eq!(count, 0, "transaction rollback should remove data");

    Ok(())
}

#[tokio::test]
async fn test_transaction_aware_helpers() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Use the transaction-aware helper methods
    let tenant_id = env.create_tenant_tx(&mut tx, "tx-helper-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Verify data exists within the transaction
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(tenant_count, 1, "tenant should exist in transaction");

    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(endpoint_count, 1, "endpoint should exist in transaction");

    // Explicit rollback to prevent connection leak
    tx.rollback().await?;

    // Verify data was rolled back
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(tenant_count, 0, "tenant should be rolled back");

    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(endpoint_count, 0, "endpoint should be rolled back");

    Ok(())
}

#[tokio::test]
async fn test_transaction_aware_helpers_with_transaction() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // Transaction-aware helpers work with transactions
    let mut tx = env.pool().begin().await?;
    let unique_name = format!("pool-tenant-{}", Uuid::new_v4().simple());
    let tenant_id = env.create_tenant_tx(&mut tx, &unique_name).await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Verify data exists (not in a transaction, so it persists)
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(tenant_count, 1, "tenant should exist");

    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(endpoint_count, 1, "endpoint should exist");

    // Manual cleanup
    sqlx::query("DELETE FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .execute(env.pool())
        .await?;
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id.0).execute(env.pool()).await?;

    Ok(())
}

#[tokio::test]
async fn test_transaction_rollback_isolation() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // Create a transaction and insert test data
    let tenant_id = {
        let mut tx = env.pool().begin().await?;

        let tenant_id =
            env.create_tenant_with_plan_tx(&mut tx, "rollback-test", "enterprise").await?;

        // Verify data exists within transaction
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
            .bind(tenant_id.0)
            .fetch_one(&mut *tx)
            .await?;

        assert_eq!(count, 1, "tenant should exist within transaction");

        // Rollback instead of commit
        tx.rollback().await?;
        tenant_id
    };

    // Verify data does not exist after rollback
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;

    assert_eq!(count, 0, "tenant should not exist after rollback");

    Ok(())
}

#[tokio::test]
async fn test_nested_transactions() -> Result<()> {
    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Create tenant in main transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "main-tx-tenant").await?;

    // Create savepoint using raw SQL
    sqlx::query("SAVEPOINT test_savepoint").execute(&mut *tx).await?;

    // Create endpoint in savepoint
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Verify both exist in savepoint
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1);

    // Rollback savepoint - this should remove endpoint but keep tenant
    sqlx::query("ROLLBACK TO SAVEPOINT test_savepoint").execute(&mut *tx).await?;

    // Tenant should still exist in main transaction
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(tenant_count, 1);

    // Endpoint should not exist after savepoint rollback
    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(endpoint_count, 0);

    // Rollback main transaction for cleanup
    tx.rollback().await?;

    Ok(())
}

#[tokio::test]
async fn test_concurrent_transactions_on_same_pool() -> Result<()> {
    let env = TestEnv::new_isolated().await?;

    // Start two concurrent transactions from the same pool
    let mut tx1 = env.pool().begin().await?;
    let mut tx2 = env.pool().begin().await?;

    // Each transaction creates its own tenant
    let tenant1 = env.create_tenant_tx(&mut tx1, "tx1-tenant").await?;
    let tenant2 = env.create_tenant_tx(&mut tx2, "tx2-tenant").await?;

    // Neither transaction can see the other's uncommitted data
    let count1: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant2.0)
        .fetch_one(&mut *tx1)
        .await?;
    assert_eq!(count1, 0, "tx1 should not see tx2's uncommitted data");

    let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant1.0)
        .fetch_one(&mut *tx2)
        .await?;
    assert_eq!(count2, 0, "tx2 should not see tx1's uncommitted data");

    // Commit tx1
    tx1.commit().await?;

    // tx2 can now see tx1's committed data (READ COMMITTED isolation)
    let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant1.0)
        .fetch_one(&mut *tx2)
        .await?;
    assert_eq!(count2, 1, "tx2 can see tx1's committed data in READ COMMITTED isolation");

    // Rollback tx2
    tx2.rollback().await?;

    // New query should see tx1's committed data but not tx2's rolled-back data
    let count1_final: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant1.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(count1_final, 1, "tx1's data should be persisted");

    let count2_final: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant2.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(count2_final, 0, "tx2's data should not exist");

    Ok(())
}

#[tokio::test]
async fn test_transaction_with_webhook_ingestion() -> Result<()> {
    use kapsel_testing::fixtures::WebhookBuilder;

    let env = TestEnv::new_isolated().await?;
    let mut tx = env.pool().begin().await?;

    // Create test data within transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "webhook-tx-tenant").await?;
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Create and ingest webhook within transaction
    let webhook = WebhookBuilder::new()
        .tenant(tenant_id.0)
        .endpoint(endpoint_id.0)
        .source_event("tx-test-event")
        .body(b"transaction test".to_vec())
        .build();

    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await?;

    // Verify webhook exists within transaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 1, "webhook should exist within transaction");

    // Rollback
    tx.rollback().await?;

    // Verify everything was rolled back
    let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(event_count, 0, "webhook should be rolled back");

    Ok(())
}
