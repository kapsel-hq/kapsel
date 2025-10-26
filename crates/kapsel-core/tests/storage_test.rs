//! Integration tests for storage repositories.
//!
//! Tests all database operations using the production Storage repositories
//! to ensure correctness of SQL queries and data integrity.

use std::collections::HashMap;

use chrono::Utc;
use kapsel_core::{
    models::{CircuitState, DeliveryAttempt, EventStatus},
    storage::Storage,
};
use kapsel_testing::{events::test_events, TestEnv};
use uuid::Uuid;

#[tokio::test]
async fn storage_health_check() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    // Health check should succeed
    assert!(storage.health_check().await.is_ok());
}

#[tokio::test]
async fn tenant_repository_crud_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    // Create tenant using the repository
    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();

    tx.commit().await.unwrap();

    // Test find by ID
    let found = storage.tenants.find_by_id(tenant_id).await.unwrap();
    assert!(found.is_some());
    let tenant = found.unwrap();
    assert_eq!(tenant.id, tenant_id);
    assert!(tenant.name.contains("test-tenant"));

    // Test find by name
    let found_by_name = storage.tenants.find_by_name(&tenant.name).await.unwrap();
    assert!(found_by_name.is_some());
    assert_eq!(found_by_name.unwrap().id, tenant_id);

    // Test exists
    assert!(storage.tenants.exists(tenant_id).await.unwrap());

    // Test name exists
    assert!(storage.tenants.name_exists(&tenant.name).await.unwrap());

    // Test update tier
    storage.tenants.update_tier(tenant_id, "enterprise").await.unwrap();
    let updated = storage.tenants.find_by_id(tenant_id).await.unwrap().unwrap();
    assert_eq!(updated.tier, "enterprise");
}

#[tokio::test]
async fn endpoint_repository_crud_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    tx.commit().await.unwrap();

    // Test find by ID
    let found = storage.endpoints.find_by_id(endpoint_id).await.unwrap();
    assert!(found.is_some());
    let endpoint = found.unwrap();
    assert_eq!(endpoint.id, endpoint_id);
    assert_eq!(endpoint.tenant_id, tenant_id);
    assert_eq!(endpoint.url, "https://example.com/webhook");
    assert!(endpoint.is_active);

    // Test find by tenant
    let endpoints = storage.endpoints.find_by_tenant(tenant_id).await.unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].id, endpoint_id);

    // Test find active by tenant
    let active = storage.endpoints.find_active_by_tenant(tenant_id).await.unwrap();
    assert_eq!(active.len(), 1);

    // Test set enabled/disabled
    storage.endpoints.set_enabled(endpoint_id, false).await.unwrap();
    let disabled = storage.endpoints.find_by_id(endpoint_id).await.unwrap().unwrap();
    assert!(!disabled.is_active);

    // Active endpoints should now be empty
    let active = storage.endpoints.find_active_by_tenant(tenant_id).await.unwrap();
    assert_eq!(active.len(), 0);

    // Test update circuit state
    let now = Utc::now();
    storage
        .endpoints
        .update_circuit_state(endpoint_id, CircuitState::Open, 5, 0, Some(now), None)
        .await
        .unwrap();

    let updated = storage.endpoints.find_by_id(endpoint_id).await.unwrap().unwrap();
    assert_eq!(updated.circuit_state, CircuitState::Open);
    assert_eq!(updated.circuit_failure_count, 5);
    assert_eq!(updated.circuit_success_count, 0);
    assert!(updated.circuit_last_failure_at.is_some());

    // Test count operations
    let count = storage.endpoints.count_by_tenant(tenant_id).await.unwrap();
    assert_eq!(count, 1);

    let active_count = storage.endpoints.count_active_by_tenant(tenant_id).await.unwrap();
    assert_eq!(active_count, 0); // Disabled earlier
}

#[tokio::test]
async fn webhook_event_repository_crud_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create a webhook event using fixtures
    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await.unwrap();

    tx.commit().await.unwrap();

    // Test find by ID
    let found = storage.webhook_events.find_by_id(event_id).await.unwrap();
    assert!(found.is_some());
    let event = found.unwrap();
    assert_eq!(event.id, event_id);
    assert_eq!(event.tenant_id, tenant_id);
    assert_eq!(event.endpoint_id, endpoint_id);
    assert_eq!(event.status, EventStatus::Pending);

    // Test find duplicate
    let duplicate =
        storage.webhook_events.find_duplicate(endpoint_id, &webhook.source_event_id).await.unwrap();
    assert!(duplicate.is_some());
    assert_eq!(duplicate.unwrap().id, event_id);

    // Test find by tenant
    let events = storage.webhook_events.find_by_tenant(tenant_id, Some(10)).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);

    // Test find by endpoint
    let events = storage.webhook_events.find_by_endpoint(endpoint_id, Some(10)).await.unwrap();
    assert_eq!(events.len(), 1);

    // Test count by status
    let pending_count = storage.webhook_events.count_by_status(EventStatus::Pending).await.unwrap();
    assert_eq!(pending_count, 1);

    // Test count by tenant
    let tenant_count = storage.webhook_events.count_by_tenant(tenant_id).await.unwrap();
    assert_eq!(tenant_count, 1);
}

#[tokio::test]
async fn webhook_event_claim_and_delivery_flow() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create multiple events
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let webhook = test_events::create_test_webhook_with_payload(
            tenant_id,
            endpoint_id,
            &format!("{{\"message\": \"test{}\"}}", i),
        );
        let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await.unwrap();
        event_ids.push(event_id);
    }

    tx.commit().await.unwrap();

    // Test claim pending events
    let claimed = storage.webhook_events.claim_pending(2).await.unwrap();
    assert_eq!(claimed.len(), 2);

    // All claimed events should have status 'delivering'
    for event in &claimed {
        assert_eq!(event.status, EventStatus::Delivering);
        assert!(event.last_attempt_at.is_some());
    }

    // Test mark as delivered
    storage.webhook_events.mark_delivered(event_ids[0]).await.unwrap();
    let delivered = storage.webhook_events.find_by_id(event_ids[0]).await.unwrap().unwrap();
    assert_eq!(delivered.status, EventStatus::Delivered);
    assert!(delivered.delivered_at.is_some());
    // Test mark failed with retry
    let next_retry = Utc::now() + chrono::Duration::seconds(60);
    storage.webhook_events.mark_failed(claimed[1].id, 1, Some(next_retry)).await.unwrap();

    let failed = storage.webhook_events.find_by_id(claimed[1].id).await.unwrap().unwrap();
    assert_eq!(failed.status, EventStatus::Pending); // Should be pending for retry
    assert_eq!(failed.failure_count, 1);
    assert!(failed.next_retry_at.is_some());

    // Test mark permanently failed
    storage.webhook_events.mark_failed(claimed[1].id, 4, None).await.unwrap();
    let permanently_failed =
        storage.webhook_events.find_by_id(claimed[1].id).await.unwrap().unwrap();
    assert_eq!(permanently_failed.status, EventStatus::Failed);
    assert!(permanently_failed.failed_at.is_some());

    // Verify counts by status
    let delivered_count =
        storage.webhook_events.count_by_status(EventStatus::Delivered).await.unwrap();
    assert_eq!(delivered_count, 1);

    let failed_count = storage.webhook_events.count_by_status(EventStatus::Failed).await.unwrap();
    assert_eq!(failed_count, 1);

    let pending_count = storage.webhook_events.count_by_status(EventStatus::Pending).await.unwrap();
    assert_eq!(pending_count, 1); // One remaining unclaimed event
}

#[tokio::test]
async fn concurrent_event_claiming() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create 5 events
    for i in 0..5 {
        let webhook = test_events::create_test_webhook_with_payload(
            tenant_id,
            endpoint_id,
            &format!("{{\"message\": \"concurrent{}\"}}", i),
        );
        env.ingest_webhook_tx(&mut *tx, &webhook).await.unwrap();
    }

    tx.commit().await.unwrap();

    // Spawn two concurrent claim operations
    let storage1 = storage.clone();
    let storage2 = storage.clone();

    let handle1: tokio::task::JoinHandle<Vec<kapsel_core::models::WebhookEvent>> =
        tokio::spawn(async move { storage1.webhook_events.claim_pending(3).await.unwrap() });

    let handle2: tokio::task::JoinHandle<Vec<kapsel_core::models::WebhookEvent>> =
        tokio::spawn(async move { storage2.webhook_events.claim_pending(3).await.unwrap() });

    let results1: Vec<kapsel_core::models::WebhookEvent> = handle1.await.unwrap();
    let results2: Vec<kapsel_core::models::WebhookEvent> = handle2.await.unwrap();

    // Verify no overlap between claimed events (SKIP LOCKED should prevent this)
    let ids1: Vec<_> = results1.iter().map(|e| e.id).collect();
    let ids2: Vec<_> = results2.iter().map(|e| e.id).collect();

    for id in &ids1 {
        assert!(!ids2.contains(id), "Event {:?} claimed by both workers", id);
    }

    // Total should be 5 (all events claimed)
    assert_eq!(ids1.len() + ids2.len(), 5);
}

#[tokio::test]
async fn delivery_attempt_repository_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut *tx, tenant_id, "https://example.com/webhook").await.unwrap();

    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook_tx(&mut *tx, &webhook).await.unwrap();

    tx.commit().await.unwrap();

    // Create delivery attempt
    let mut request_headers = HashMap::new();
    request_headers.insert("User-Agent".to_string(), "Kapsel/1.0".to_string());

    let mut response_headers = HashMap::new();
    response_headers.insert("Content-Type".to_string(), "text/plain".to_string());

    let attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 1,
        endpoint_id,
        request_headers: request_headers.clone(),
        request_body: b"test request".to_vec(),
        response_status: Some(200),
        response_headers: Some(response_headers.clone()),
        response_body: Some(b"OK".to_vec()),
        attempted_at: Utc::now(),
        succeeded: true,
        error_message: None,
    };

    // Test create
    let attempt_id = storage.delivery_attempts.create(&attempt).await.unwrap();
    assert_eq!(attempt_id, attempt.id);

    // Test find by event
    let attempts = storage.delivery_attempts.find_by_event(event_id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, attempt.id);
    assert_eq!(attempts[0].attempt_number, 1);
    assert!(attempts[0].succeeded);

    // Test find latest by event
    let latest = storage.delivery_attempts.find_latest_by_event(event_id).await.unwrap();
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().id, attempt.id);

    // Test count operations
    let count = storage.delivery_attempts.count_by_event(event_id).await.unwrap();
    assert_eq!(count, 1);

    let successful = storage.delivery_attempts.count_successful_by_event(event_id).await.unwrap();
    assert_eq!(successful, 1);

    // Test success rate
    let rate = storage.delivery_attempts.success_rate_by_endpoint(endpoint_id, None).await.unwrap();
    assert_eq!(rate, 1.0);

    // Add a failed attempt
    let failed_attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 2,
        endpoint_id,
        request_headers,
        request_body: b"test request 2".to_vec(),
        response_status: Some(500),
        response_headers: Some(response_headers),
        response_body: Some(b"Internal Server Error".to_vec()),
        attempted_at: Utc::now(),
        succeeded: false,
        error_message: Some("Server error".to_string()),
    };

    storage.delivery_attempts.create(&failed_attempt).await.unwrap();

    // Test success rate with mixed results
    let rate = storage.delivery_attempts.success_rate_by_endpoint(endpoint_id, None).await.unwrap();
    assert_eq!(rate, 0.5); // 1 success out of 2 attempts

    // Test find by endpoint
    let endpoint_attempts =
        storage.delivery_attempts.find_by_endpoint(endpoint_id, Some(10)).await.unwrap();
    assert_eq!(endpoint_attempts.len(), 2);

    // Test recent failures
    let failures = storage
        .delivery_attempts
        .find_recent_failures_by_endpoint(
            endpoint_id,
            Utc::now() - chrono::Duration::hours(1),
            Some(10),
        )
        .await
        .unwrap();
    assert_eq!(failures.len(), 1); // Only the failed attempt
    assert_eq!(failures[0].id, failed_attempt.id);
}

#[tokio::test]
async fn transactional_operations_rollback() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    // Create tenant in a transaction that will be rolled back
    let tenant_id = {
        let mut tx = env.pool().begin().await.unwrap();
        let tenant_id = env.create_tenant_tx(&mut *tx, "rollback-test-tenant").await.unwrap();

        // Explicitly rollback the transaction
        tx.rollback().await.unwrap();
        tenant_id
    };

    // Tenant should not exist after rollback
    let found_after_rollback = storage.tenants.find_by_id(tenant_id).await.unwrap();
    assert!(found_after_rollback.is_none());
}

#[tokio::test]
async fn constraint_violation_handling() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut *tx, "constraint-tenant").await.unwrap();

    tx.commit().await.unwrap();

    let tenant = storage.tenants.find_by_id(tenant_id).await.unwrap().unwrap();

    // Try to create another tenant with the same name (should fail due to unique
    // constraint)
    let result = storage.tenants.create(&tenant).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn system_tenant_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    // Ensure system tenant exists
    let system_id = storage.tenants.ensure_system_tenant().await.unwrap();

    // Should be the nil UUID
    assert_eq!(system_id.0, Uuid::nil());

    // Should find the system tenant
    let system_tenant = storage.tenants.find_by_id(system_id).await.unwrap();
    assert!(system_tenant.is_some());

    let tenant = system_tenant.unwrap();
    assert_eq!(tenant.name, "system");
    assert_eq!(tenant.tier, "system");

    // Calling again should not fail (ON CONFLICT handling)
    let system_id2 = storage.tenants.ensure_system_tenant().await.unwrap();
    assert_eq!(system_id, system_id2);
}

#[tokio::test]
async fn storage_repository_isolation() {
    let env = TestEnv::new_isolated().await.unwrap();
    let storage = Storage::new(env.pool().clone());

    let mut tx = env.pool().begin().await.unwrap();

    let tenant1_id = env.create_tenant_tx(&mut *tx, "tenant1").await.unwrap();
    let tenant2_id = env.create_tenant_tx(&mut *tx, "tenant2").await.unwrap();

    tx.commit().await.unwrap();

    let mut tx2 = env.pool().begin().await.unwrap();
    let endpoint1_id =
        env.create_endpoint_tx(&mut *tx2, tenant1_id, "https://tenant1.com").await.unwrap();
    let endpoint2_id =
        env.create_endpoint_tx(&mut *tx2, tenant2_id, "https://tenant2.com").await.unwrap();

    tx2.commit().await.unwrap();

    // Verify tenant isolation
    let tenant1_endpoints = storage.endpoints.find_by_tenant(tenant1_id).await.unwrap();
    let tenant2_endpoints = storage.endpoints.find_by_tenant(tenant2_id).await.unwrap();

    assert_eq!(tenant1_endpoints.len(), 1);
    assert_eq!(tenant2_endpoints.len(), 1);
    assert_eq!(tenant1_endpoints[0].id, endpoint1_id);
    assert_eq!(tenant2_endpoints[0].id, endpoint2_id);

    // Cross-tenant queries should return empty results
    assert!(tenant1_endpoints.iter().all(|e| e.tenant_id == tenant1_id));
    assert!(tenant2_endpoints.iter().all(|e| e.tenant_id == tenant2_id));
}
