//! Tests for transaction isolation in TestEnv.
//!
//! These tests verify that transaction-based isolation works correctly with
//! the shared database pool, proving that tests can safely share a database
//! when using proper transaction boundaries.

use anyhow::Result;
use futures;
use kapsel_testing::TestEnv;
use uuid::Uuid;

#[tokio::test]
async fn test_transactions_provide_isolation() -> Result<()> {
    // Both environments use the shared database pool
    let env = TestEnv::new().await?;

    // Each begins its own transaction on the shared pool
    let mut tx1 = env.pool().begin().await?;
    let mut tx2 = env.pool().begin().await?;

    // tx1 inserts data
    let tenant_id1 = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, tier) VALUES ($1, $2, $3)")
        .bind(tenant_id1)
        .bind("tenant-tx1")
        .bind("free")
        .execute(&mut *tx1)
        .await?;

    // tx2 should NOT see tx1's uncommitted data
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id1)
        .fetch_one(&mut *tx2)
        .await?;
    assert_eq!(count, 0, "tx2 should not see uncommitted data from tx1");

    // Transactions automatically roll back when dropped
    // No explicit rollback needed, but we can be explicit for clarity
    tx1.rollback().await?;
    tx2.rollback().await?;

    Ok(())
}

#[tokio::test]
async fn test_rollback_prevents_data_persistence() -> Result<()> {
    let env = TestEnv::new().await?;

    let tenant_id = {
        let mut tx = env.pool().begin().await?;

        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, tier) VALUES ($1, $2, $3)")
            .bind(id)
            .bind("rollback-test")
            .bind("free")
            .execute(&mut *tx)
            .await?;

        tx.rollback().await?;
        id
    };

    // Check using the same pool - data should not exist after rollback
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(env.pool())
        .await?;

    assert_eq!(count, 0, "rolled back data should not persist");

    Ok(())
}

#[tokio::test]
async fn test_helper_methods_work_with_transactions() -> Result<()> {
    let env = TestEnv::new().await?;
    let mut tx = env.pool().begin().await?;

    // Test that we can create tenant within a transaction
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $2, $3, NOW(), NOW())")
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

    // Transaction rolls back automatically when dropped
    drop(tx);

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
    let env = TestEnv::new().await?;
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

    // Let transaction roll back automatically
    drop(tx);

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
async fn test_transaction_commit_persists_data() -> Result<()> {
    let env = TestEnv::new().await?;

    // Create unique IDs to avoid conflicts in shared database
    let unique_suffix = Uuid::new_v4().simple().to_string();
    let tenant_name = format!("commit-test-{}", unique_suffix);

    let (tenant_id, endpoint_id) = {
        let mut tx = env.pool().begin().await?;
        let tenant_id = env.create_tenant_tx(&mut tx, &tenant_name).await?;
        let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

        // Explicitly commit the transaction
        tx.commit().await?;

        (tenant_id, endpoint_id)
    };

    // Verify data persists after commit
    let tenant_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(tenant_count, 1, "tenant should exist after commit");

    let endpoint_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(endpoint_count, 1, "endpoint should exist after commit");

    // Clean up the committed data
    let mut cleanup_tx = env.pool().begin().await?;
    sqlx::query("DELETE FROM endpoints WHERE id = $1")
        .bind(endpoint_id.0)
        .execute(&mut *cleanup_tx)
        .await?;
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id.0)
        .execute(&mut *cleanup_tx)
        .await?;
    cleanup_tx.commit().await?;

    Ok(())
}

#[tokio::test]
async fn test_transaction_rollback_isolation() -> Result<()> {
    let env = TestEnv::new().await?;

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
async fn test_savepoints() -> Result<()> {
    let env = TestEnv::new().await?;
    let mut tx = env.pool().begin().await?;

    // Create tenant in main transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "main-tx-tenant").await?;

    // Create savepoint using raw SQL
    sqlx::query("SAVEPOINT test_savepoint").execute(&mut *tx).await?;

    // Create endpoint in savepoint
    let endpoint_id = env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com").await?;

    // Verify both exist in current transaction state
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

    // Let main transaction roll back for cleanup
    drop(tx);

    Ok(())
}

#[tokio::test]
async fn test_concurrent_transactions_on_shared_pool() -> Result<()> {
    let env = TestEnv::new().await?;

    // Start two concurrent transactions from the same shared pool
    let mut tx1 = env.pool().begin().await?;
    let mut tx2 = env.pool().begin().await?;

    // Each transaction creates its own tenant with unique names
    let suffix = Uuid::new_v4().simple().to_string();
    let tenant1 = env.create_tenant_tx(&mut tx1, &format!("tx1-tenant-{}", suffix)).await?;
    let tenant2 = env.create_tenant_tx(&mut tx2, &format!("tx2-tenant-{}", suffix)).await?;

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

    // tx2 still can't see tx1's data in its snapshot (REPEATABLE READ)
    let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
        .bind(tenant1.0)
        .fetch_one(&mut *tx2)
        .await?;
    assert_eq!(count2, 0, "tx2 maintains its snapshot isolation");

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

    // Clean up tx1's committed data
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant1.0).execute(env.pool()).await?;

    Ok(())
}

#[tokio::test]
async fn test_transaction_with_webhook_ingestion() -> Result<()> {
    use kapsel_testing::fixtures::WebhookBuilder;

    let env = TestEnv::new().await?;
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

    // Let transaction roll back automatically
    drop(tx);

    // Verify everything was rolled back
    let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
        .bind(event_id.0)
        .fetch_one(env.pool())
        .await?;
    assert_eq!(event_count, 0, "webhook should be rolled back");

    Ok(())
}

#[tokio::test]
async fn test_shared_pool_isolation_guarantees() -> Result<()> {
    // This test proves that the shared database pattern provides
    // complete isolation when used correctly with transactions

    let env = TestEnv::new().await?;

    // Run 10 concurrent transactions to stress test isolation
    let mut handles = vec![];

    for i in 0..10 {
        let pool = env.pool().clone();

        let handle = tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();

            // Each transaction creates its own unique tenant
            let tenant_id = Uuid::new_v4();
            let name = format!("concurrent-tenant-{}-{}", i, tenant_id.simple());

            sqlx::query(
                "INSERT INTO tenants (id, name, tier, created_at, updated_at)
                 VALUES ($1, $2, $3, NOW(), NOW())",
            )
            .bind(tenant_id)
            .bind(&name)
            .bind("free")
            .execute(&mut *tx)
            .await
            .unwrap();

            // Verify we can see our own data
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
                .bind(tenant_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            assert_eq!(count, 1);

            // Small delay to ensure transactions overlap
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            // Roll back - no data should persist
            tx.rollback().await.unwrap();

            tenant_id
        });

        handles.push(handle);
    }

    // Wait for all transactions to complete
    let tenant_ids: Vec<Uuid> =
        futures::future::join_all(handles).await.into_iter().map(|r| r.unwrap()).collect();

    // Verify none of the rolled-back data persists
    for tenant_id in tenant_ids {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(env.pool())
            .await?;
        assert_eq!(count, 0, "rolled back tenant should not exist");
    }

    Ok(())
}
