//! Integration tests for storage repositories.
//!
//! Tests all database operations using the production Storage repositories
//! to ensure correctness of SQL queries and data integrity.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use kapsel_core::{
    models::{CircuitState, DeliveryAttempt, EventStatus, Tenant, TenantId},
    storage::{endpoints::CircuitStateUpdate, Storage},
    Clock, TestClock,
};
use kapsel_testing::{events::test_events, TestEnv};
use uuid::Uuid;

#[tokio::test]
async fn storage_health_check() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    // Health check should succeed
    assert!(storage.health_check().await.is_ok());
}

#[tokio::test]
async fn tenant_repository_crud_operations() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Create tenant using transaction helper
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();

    // Test find by ID
    let found = env.find_tenant_by_id_tx(&mut tx, tenant_id).await.unwrap();
    assert!(found.is_some());
    let tenant = found.unwrap();
    assert_eq!(tenant.id, tenant_id);
    assert!(tenant.name.contains("test-tenant"));

    // Test find by name
    let found_by_name = env.find_tenant_by_name_tx(&mut tx, &tenant.name).await.unwrap();
    assert!(found_by_name.is_some());
    assert_eq!(found_by_name.unwrap().id, tenant_id);

    // Test exists
    assert!(env.tenant_exists_tx(&mut tx, tenant_id).await.unwrap());

    // Test name exists
    assert!(env.tenant_name_exists_tx(&mut tx, &tenant.name).await.unwrap());

    // Test tier operations within transaction using helpers
    let initial_tier = env.get_tenant_tier_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(initial_tier, "free"); // default tier

    // Test update tier using helper
    env.update_tenant_tier_tx(&mut tx, tenant_id, "enterprise").await.unwrap();

    let updated_tier = env.get_tenant_tier_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(updated_tier, "enterprise");
}

#[tokio::test]
async fn endpoint_repository_crud_operations() {
    let env = TestEnv::new_shared().await.unwrap();
    let mut tx = env.pool().begin().await.unwrap();

    // Create test data within transaction
    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Test find by ID
    let found = env.find_endpoint_by_id_tx(&mut tx, endpoint_id).await.unwrap();
    assert!(found.is_some());
    let endpoint = found.unwrap();
    assert_eq!(endpoint.id, endpoint_id);
    assert_eq!(endpoint.tenant_id, tenant_id);
    assert_eq!(endpoint.url, "https://example.com/webhook");
    assert!(endpoint.is_active);

    // Test find by tenant
    let endpoints = env.find_endpoints_by_tenant_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].id, endpoint_id);

    // Test find active by tenant
    let active = env.find_active_endpoints_by_tenant_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(active.len(), 1);

    // Test set enabled/disabled
    env.set_endpoint_enabled_tx(&mut tx, endpoint_id, false).await.unwrap();
    let disabled = env.find_endpoint_by_id_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert!(!disabled.is_active);

    // Active endpoints should now be empty
    let active = env.find_active_endpoints_by_tenant_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(active.len(), 0);

    // Test circuit breaker operations within transaction
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Open, 5, 0).await.unwrap();
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::HalfOpen, 2, 1)
        .await
        .unwrap();
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Closed, 0, 3).await.unwrap();
}

#[tokio::test]
async fn webhook_event_repository_crud_operations() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create a webhook event using fixtures
    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

    // Test find by ID within transaction
    let found = env.find_webhook_event_by_id_tx(&mut tx, event_id).await.unwrap();
    assert!(found.is_some());
    let event = found.unwrap();
    assert_eq!(event.id, event_id);
    assert_eq!(event.tenant_id, tenant_id);
    assert_eq!(event.endpoint_id, endpoint_id);
    assert_eq!(event.status, EventStatus::Pending);

    // Test find duplicate within transaction
    let duplicate = env
        .find_webhook_event_duplicate_tx(&mut tx, endpoint_id, &webhook.source_event_id)
        .await
        .unwrap();
    assert!(duplicate.is_some());
    assert_eq!(duplicate.unwrap().id, event_id);

    // Test find by tenant within transaction
    let events = env.find_webhook_events_by_tenant_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);

    // Test count by tenant within transaction
    let tenant_count = env.count_webhook_events_by_tenant_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(tenant_count, 1);

    // Test status update within transaction
    env.update_webhook_event_status_tx(&mut tx, event_id, EventStatus::Delivering).await.unwrap();
    let updated = env.find_webhook_event_by_id_tx(&mut tx, event_id).await.unwrap().unwrap();
    assert_eq!(updated.status, EventStatus::Delivering);
}

#[tokio::test]
async fn webhook_event_claim_and_delivery_flow() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create multiple events within the same transaction
    let mut event_ids = Vec::new();
    for i in 0..3 {
        let webhook = test_events::create_test_webhook_with_payload(
            tenant_id,
            endpoint_id,
            &format!("{{\"message\": \"test{i}\"}}"),
        );
        let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();
        event_ids.push(event_id);
    }

    // Test claim pending events within transaction
    let claimed = storage.webhook_events.claim_pending_in_tx(&mut tx, 2).await.unwrap();
    assert_eq!(claimed.len(), 2);

    // All claimed events should have status 'delivering'
    for event in &claimed {
        assert_eq!(event.status, EventStatus::Delivering);
        assert!(event.last_attempt_at.is_some());
    }

    // Test mark as delivered
    storage.webhook_events.mark_delivered_in_tx(&mut tx, event_ids[0]).await.unwrap();
    let delivered =
        storage.webhook_events.find_by_id_in_tx(&mut tx, event_ids[0]).await.unwrap().unwrap();
    assert_eq!(delivered.status, EventStatus::Delivered);
    assert!(delivered.delivered_at.is_some());

    // Test mark failed with retry
    let next_retry = DateTime::<Utc>::from(env.clock.now_system()) + chrono::Duration::seconds(60);
    storage
        .webhook_events
        .mark_failed_in_tx(&mut tx, claimed[1].id, 1, Some(next_retry))
        .await
        .unwrap();

    let failed =
        storage.webhook_events.find_by_id_in_tx(&mut tx, claimed[1].id).await.unwrap().unwrap();
    assert_eq!(failed.status, EventStatus::Pending); // Should be pending for retry
    assert_eq!(failed.failure_count, 1);
    assert!(failed.next_retry_at.is_some());

    // Test mark permanently failed
    storage.webhook_events.mark_failed_in_tx(&mut tx, claimed[1].id, 4, None).await.unwrap();
    let permanently_failed =
        storage.webhook_events.find_by_id_in_tx(&mut tx, claimed[1].id).await.unwrap().unwrap();
    assert_eq!(permanently_failed.status, EventStatus::Failed);
    assert!(permanently_failed.failed_at.is_some());

    // Verify counts by status
    let delivered_count = storage
        .webhook_events
        .count_by_status_in_tx(&mut tx, EventStatus::Delivered)
        .await
        .unwrap();
    assert_eq!(delivered_count, 1);

    let failed_count =
        storage.webhook_events.count_by_status_in_tx(&mut tx, EventStatus::Failed).await.unwrap();
    assert_eq!(failed_count, 1);

    let pending_count =
        storage.webhook_events.count_by_status_in_tx(&mut tx, EventStatus::Pending).await.unwrap();
    assert_eq!(pending_count, 1); // One remaining unclaimed event
}

#[tokio::test]
async fn concurrent_event_claiming() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "concurrent-test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create 5 events within transaction
    for i in 0..5 {
        let webhook = test_events::create_test_webhook_with_payload(
            tenant_id,
            endpoint_id,
            &format!("{{\"message\": \"concurrent{i}\"}}"),
        );
        env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();
    }

    // Commit so concurrent claiming operations can see the events
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
        assert!(!ids2.contains(id), "Event {id:?} claimed by both workers");
    }

    // Total should be 5 (all events claimed)
    assert_eq!(ids1.len() + ids2.len(), 5);
}

#[tokio::test]
async fn delivery_attempt_repository_operations() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "test-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create a webhook event for the delivery attempt
    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

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
        attempted_at: DateTime::<Utc>::from(env.clock.now_system()),
        succeeded: true,
        error_message: None,
    };

    // Test create
    let attempt_id = storage.delivery_attempts.create_in_tx(&mut tx, &attempt).await.unwrap();
    assert_eq!(attempt_id, attempt.id);

    // Test find by event
    let attempts = storage.delivery_attempts.find_by_event_in_tx(&mut tx, event_id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, attempt.id);
    assert_eq!(attempts[0].attempt_number, 1);
    assert!(attempts[0].succeeded);

    // Test find latest by event
    let latest =
        storage.delivery_attempts.find_latest_by_event_in_tx(&mut tx, event_id).await.unwrap();
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().id, attempt.id);

    // Test count operations
    let count = storage.delivery_attempts.count_by_event_in_tx(&mut tx, event_id).await.unwrap();
    assert_eq!(count, 1);

    let successful =
        storage.delivery_attempts.count_successful_by_event_in_tx(&mut tx, event_id).await.unwrap();
    assert_eq!(successful, 1);

    // Test success rate
    let rate = storage
        .delivery_attempts
        .success_rate_by_endpoint_in_tx(&mut tx, endpoint_id, None)
        .await
        .unwrap();
    assert!((rate - 1.0).abs() < f64::EPSILON); // 1 success out of 1 attempt

    // Add a failed attempt
    let failed_attempt = DeliveryAttempt {
        id: Uuid::new_v4(),
        event_id,
        attempt_number: 2,
        endpoint_id,
        request_headers,
        request_body: b"test request 2".to_vec(),
        response_status: Some(404),
        response_headers: Some(response_headers),
        response_body: Some(b"Not Found".to_vec()),
        attempted_at: DateTime::<Utc>::from(env.clock.now_system()),
        succeeded: false,
        error_message: Some("Server error".to_string()),
    };

    storage.delivery_attempts.create_in_tx(&mut tx, &failed_attempt).await.unwrap();

    // Test success rate with mixed results
    let rate = storage
        .delivery_attempts
        .success_rate_by_endpoint_in_tx(&mut tx, endpoint_id, None)
        .await
        .unwrap();
    assert!((rate - 0.5).abs() < f64::EPSILON); // 1 success out of 2 attempts

    // Test find by endpoint
    let endpoint_attempts = storage
        .delivery_attempts
        .find_by_endpoint_in_tx(&mut tx, endpoint_id, Some(10))
        .await
        .unwrap();
    assert_eq!(endpoint_attempts.len(), 2);

    // Test recent failures
    let failures = storage
        .delivery_attempts
        .find_recent_failures_by_endpoint_in_tx(
            &mut tx,
            endpoint_id,
            DateTime::<Utc>::from(env.clock.now_system()) - chrono::Duration::hours(1),
            Some(10),
        )
        .await
        .unwrap();
    assert_eq!(failures.len(), 1); // Only the failed attempt
    assert_eq!(failures[0].id, failed_attempt.id);
}

#[tokio::test]
async fn transactional_operations_rollback() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    // Create tenant in a transaction that will be rolled back
    let tenant_id = {
        let mut tx = env.pool().begin().await.unwrap();
        let tenant_id = env.create_tenant_tx(&mut tx, "rollback-test-tenant").await.unwrap();

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
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);
    let mut tx = env.pool().begin().await.unwrap();

    // Generate unique name for shared database
    let unique_suffix = Uuid::new_v4().simple().to_string();
    let tenant_name = format!("constraint-tenant-{}", unique_suffix);

    // Create first tenant using storage directly to control exact name
    let tenant1 = Tenant {
        id: TenantId::new(),
        name: tenant_name.clone(),
        tier: "free".to_string(),
        max_events_per_month: 100_000,
        max_endpoints: 100,
        events_this_month: 0,
        created_at: DateTime::<Utc>::from(clock.now_system()),
        updated_at: DateTime::<Utc>::from(clock.now_system()),
        deleted_at: None,
        stripe_customer_id: None,
        stripe_subscription_id: None,
    };

    storage.tenants.create_in_tx(&mut tx, &tenant1).await.unwrap();

    // Try to create another tenant with exactly the same name (should fail due to
    // unique constraint)
    let tenant2 = Tenant {
        id: TenantId::new(),
        name: tenant_name, // Same name - should cause constraint violation
        tier: "free".to_string(),
        max_events_per_month: 100_000,
        max_endpoints: 100,
        events_this_month: 0,
        created_at: DateTime::<Utc>::from(clock.now_system()),
        updated_at: DateTime::<Utc>::from(clock.now_system()),
        deleted_at: None,
        stripe_customer_id: None,
        stripe_subscription_id: None,
    };

    let result = storage.tenants.create_in_tx(&mut tx, &tenant2).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn system_tenant_operations() {
    let env = TestEnv::new_isolated().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

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
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant1_id = env.create_tenant_tx(&mut tx, "tenant1").await.unwrap();
    let tenant2_id = env.create_tenant_tx(&mut tx, "tenant2").await.unwrap();

    let endpoint1_id =
        env.create_endpoint_tx(&mut tx, tenant1_id, "https://tenant1.com").await.unwrap();
    let endpoint2_id =
        env.create_endpoint_tx(&mut tx, tenant2_id, "https://tenant2.com").await.unwrap();

    // Verify tenant isolation
    let tenant1_endpoints =
        storage.endpoints.find_by_tenant_in_tx(&mut tx, tenant1_id).await.unwrap();
    let tenant2_endpoints =
        storage.endpoints.find_by_tenant_in_tx(&mut tx, tenant2_id).await.unwrap();

    assert_eq!(tenant1_endpoints.len(), 1);
    assert_eq!(tenant2_endpoints.len(), 1);
    assert_eq!(tenant1_endpoints[0].id, endpoint1_id);
    assert_eq!(tenant2_endpoints[0].id, endpoint2_id);

    // Cross-tenant queries should return empty results
    assert!(tenant1_endpoints.iter().all(|e| e.tenant_id == tenant1_id));
    assert!(tenant2_endpoints.iter().all(|e| e.tenant_id == tenant2_id));

    // Transaction auto-rollbacks when dropped
}

/// Tests the complete dead-letter queue workflow.
///
/// Verifies that events can be moved to dead-letter status, retrieved,
/// and retried correctly. This is critical for reliability.
#[tokio::test]
async fn webhook_events_dead_letter_queue_workflow() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "dlq-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/webhook").await.unwrap();

    // Create a webhook event using fixtures within transaction
    let webhook = test_events::create_test_webhook(tenant_id, endpoint_id);
    let event_id = env.ingest_webhook_tx(&mut tx, &webhook).await.unwrap();

    // Step 1: Move event to failed state first
    storage.webhook_events.mark_failed_in_tx(&mut tx, event_id, 3, None).await.unwrap();

    // Verify it's failed
    let event = storage.webhook_events.find_by_id_in_tx(&mut tx, event_id).await.unwrap().unwrap();
    assert_eq!(event.status, EventStatus::Failed);

    // Step 2: Move to dead letter
    storage.webhook_events.move_to_dead_letter_in_tx(&mut tx, event_id).await.unwrap();

    // Verify status changed to dead_letter
    let event = storage.webhook_events.find_by_id_in_tx(&mut tx, event_id).await.unwrap().unwrap();
    assert_eq!(event.status, EventStatus::DeadLetter);

    // Step 3: Find dead letter by tenant
    let dead_letters = storage
        .webhook_events
        .find_dead_letter_by_tenant_in_tx(&mut tx, tenant_id, None)
        .await
        .unwrap();
    assert_eq!(dead_letters.len(), 1);
    assert_eq!(dead_letters[0].id, event_id);
    assert_eq!(dead_letters[0].status, EventStatus::DeadLetter);

    // Step 4: Retry dead letter (moves back to pending)
    storage.webhook_events.retry_dead_letter_in_tx(&mut tx, event_id).await.unwrap();

    // Verify it's back to pending
    let event = storage.webhook_events.find_by_id_in_tx(&mut tx, event_id).await.unwrap().unwrap();
    assert_eq!(event.status, EventStatus::Pending);

    // Dead letter query should now be empty
    let empty_dead_letters = storage
        .webhook_events
        .find_dead_letter_by_tenant_in_tx(&mut tx, tenant_id, None)
        .await
        .unwrap();
    assert_eq!(empty_dead_letters.len(), 0);
}

/// Tests bulk deletion of events by tenant.
///
/// This is important for data cleanup and GDPR "Right to be Forgotten".
#[tokio::test]
async fn webhook_events_delete_by_tenant() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    // Test hardcoded names - should work if transaction rollback works properly
    let tenant1_id = env.create_tenant_tx(&mut tx, "delete-tenant-1").await.unwrap();
    let tenant2_id = env.create_tenant_tx(&mut tx, "delete-tenant-2").await.unwrap();
    let endpoint1_id =
        env.create_endpoint_tx(&mut tx, tenant1_id, "https://tenant1.example.com").await.unwrap();
    let endpoint2_id =
        env.create_endpoint_tx(&mut tx, tenant2_id, "https://tenant2.example.com").await.unwrap();

    // Create events for both tenants
    let webhook1 = test_events::create_test_webhook(tenant1_id, endpoint1_id);
    let webhook2 = test_events::create_test_webhook(tenant1_id, endpoint1_id);
    let webhook3 = test_events::create_test_webhook(tenant2_id, endpoint2_id);

    env.ingest_webhook_tx(&mut tx, &webhook1).await.unwrap();
    env.ingest_webhook_tx(&mut tx, &webhook2).await.unwrap();
    env.ingest_webhook_tx(&mut tx, &webhook3).await.unwrap();

    // Verify events exist
    assert_eq!(storage.webhook_events.count_by_tenant_in_tx(&mut tx, tenant1_id).await.unwrap(), 2);
    assert_eq!(storage.webhook_events.count_by_tenant_in_tx(&mut tx, tenant2_id).await.unwrap(), 1);

    // Delete all events for tenant1
    let deleted_count =
        storage.webhook_events.delete_by_tenant_in_tx(&mut tx, tenant1_id).await.unwrap();
    assert_eq!(deleted_count, 2);

    // Verify tenant1 events are gone, tenant2 events remain
    assert_eq!(storage.webhook_events.count_by_tenant_in_tx(&mut tx, tenant1_id).await.unwrap(), 0);
    assert_eq!(storage.webhook_events.count_by_tenant_in_tx(&mut tx, tenant2_id).await.unwrap(), 1);

    // Transaction auto-rollbacks when dropped
}

/// Tests endpoint soft delete and recovery lifecycle.
///
/// Verifies that endpoints can be soft-deleted and recovered without
/// losing associated data.
#[tokio::test]
async fn endpoint_soft_delete_and_recovery() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "endpoint-lifecycle-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/lifecycle").await.unwrap();

    // Verify endpoint exists and is active
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert!(endpoint.is_active);
    assert!(endpoint.deleted_at.is_none());

    // Active endpoints query should include it
    let active_endpoints =
        storage.endpoints.find_active_by_tenant_in_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(active_endpoints.len(), 1);

    // Soft delete the endpoint
    storage.endpoints.soft_delete_in_tx(&mut tx, endpoint_id).await.unwrap();

    // Endpoint should still exist but be marked as deleted
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert!(!endpoint.is_active);
    assert!(endpoint.deleted_at.is_some());

    // Active endpoints query should not include it
    let active_endpoints =
        storage.endpoints.find_active_by_tenant_in_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(active_endpoints.len(), 0);

    // Recover the endpoint
    storage.endpoints.recover_in_tx(&mut tx, endpoint_id).await.unwrap();

    // Endpoint should be active again
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert!(endpoint.is_active);
    assert!(endpoint.deleted_at.is_none());

    // Active endpoints query should include it again
    let active_endpoints =
        storage.endpoints.find_active_by_tenant_in_tx(&mut tx, tenant_id).await.unwrap();
    assert_eq!(active_endpoints.len(), 1);
}

/// Tests endpoint statistics increment functionality.
///
/// This is critical for monitoring and analytics.
#[tokio::test]
async fn endpoint_statistics_increment() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "stats-tenant").await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/stats").await.unwrap();

    // Initial stats should be zero
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(endpoint.total_events_received, 0);
    assert_eq!(endpoint.total_events_delivered, 0);
    assert_eq!(endpoint.total_events_failed, 0);

    // Increment received events
    storage
        .endpoints
        .increment_stats_in_tx(
            &mut tx,
            endpoint_id,
            1, // events_received
            0, // events_delivered
            0, // events_failed
        )
        .await
        .unwrap();

    // Verify increment
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(endpoint.total_events_received, 1);
    assert_eq!(endpoint.total_events_delivered, 0);
    assert_eq!(endpoint.total_events_failed, 0);

    // Increment delivered events
    storage
        .endpoints
        .increment_stats_in_tx(
            &mut tx,
            endpoint_id,
            2, // events_received
            1, // events_delivered
            0, // events_failed
        )
        .await
        .unwrap();

    // Verify cumulative increments
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(endpoint.total_events_received, 3);
    assert_eq!(endpoint.total_events_delivered, 1);
    assert_eq!(endpoint.total_events_failed, 0);

    // Increment failed events
    // Increment delivered and failed events
    storage
        .endpoints
        .increment_stats_in_tx(
            &mut tx,
            endpoint_id,
            0, // events_received
            2, // events_delivered
            1, // events_failed
        )
        .await
        .unwrap();

    // Final verification
    let endpoint = storage.endpoints.find_by_id_in_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(endpoint.total_events_received, 3);
    assert_eq!(endpoint.total_events_delivered, 3);
    assert_eq!(endpoint.total_events_failed, 1);
}

/// Tests finding endpoints with open circuit breakers.
///
/// This monitoring query is essential for operational visibility.
#[tokio::test]
async fn endpoint_find_with_open_circuits() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id = env.create_tenant_tx(&mut tx, "circuit-tenant").await.unwrap();
    let endpoint1_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/circuit1").await.unwrap();
    let endpoint2_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/circuit2").await.unwrap();
    let endpoint3_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/circuit3").await.unwrap();

    // Initially no endpoints have open circuits for this tenant
    let open_circuits = storage
        .endpoints
        .find_with_open_circuits_by_tenant_in_tx(&mut tx, tenant_id, None)
        .await
        .unwrap();
    assert_eq!(open_circuits.len(), 0);

    // Set circuit breaker states
    storage
        .endpoints
        .update_circuit_state_in_tx(&mut tx, endpoint1_id, CircuitStateUpdate {
            state: CircuitState::Open,
            failure_count: 0,
            success_count: 0,
            last_failure_at: None,
            half_open_at: None,
        })
        .await
        .unwrap();
    storage
        .endpoints
        .update_circuit_state_in_tx(&mut tx, endpoint2_id, CircuitStateUpdate {
            state: CircuitState::Closed,
            failure_count: 0,
            success_count: 0,
            last_failure_at: None,
            half_open_at: None,
        })
        .await
        .unwrap();
    storage
        .endpoints
        .update_circuit_state_in_tx(&mut tx, endpoint3_id, CircuitStateUpdate {
            state: CircuitState::Open,
            failure_count: 0,
            success_count: 0,
            last_failure_at: None,
            half_open_at: None,
        })
        .await
        .unwrap();

    // Find endpoints with open circuits for this tenant
    let open_circuits = storage
        .endpoints
        .find_with_open_circuits_by_tenant_in_tx(&mut tx, tenant_id, None)
        .await
        .unwrap();
    assert_eq!(open_circuits.len(), 2);

    let open_ids: Vec<_> = open_circuits.iter().map(|e| e.id).collect();
    assert!(open_ids.contains(&endpoint1_id));
    assert!(open_ids.contains(&endpoint3_id));
    assert!(!open_ids.contains(&endpoint2_id));

    // Test with limit
    let limited = storage
        .endpoints
        .find_with_open_circuits_by_tenant_in_tx(&mut tx, tenant_id, Some(1))
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert!(limited[0].circuit_state == CircuitState::Open);
}

/// Tests manual circuit breaker reset functionality.
///
/// This is important for operational recovery procedures.
#[tokio::test]
async fn endpoint_reset_circuit_breaker() {
    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    // Generate unique tenant name
    let suffix = Uuid::new_v4().simple().to_string();
    let mut tx = env.pool().begin().await.unwrap();

    let tenant_id =
        env.create_tenant_tx(&mut tx, &format!("reset-tenant-{}", suffix)).await.unwrap();
    let endpoint_id =
        env.create_endpoint_tx(&mut tx, tenant_id, "https://example.com/reset").await.unwrap();

    // Test circuit breaker operations within transaction using helpers
    // Set circuit to open state
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Open, 5, 0).await.unwrap();

    // Verify it's open within transaction
    let endpoint = env.find_endpoint_by_id_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(endpoint.circuit_state, CircuitState::Open);

    // Reset circuit breaker to closed state
    env.update_circuit_breaker_tx(&mut tx, endpoint_id, CircuitState::Closed, 0, 0).await.unwrap();

    // Verify it's closed within transaction
    let reset_endpoint = env.find_endpoint_by_id_tx(&mut tx, endpoint_id).await.unwrap().unwrap();
    assert_eq!(reset_endpoint.circuit_state, CircuitState::Closed);
}

/// Tests API key lifecycle management for tenants.
///
/// Covers listing, counting, and deleting API keys per tenant.
#[tokio::test]
async fn api_key_tenant_lifecycle_management() {
    use chrono::{Duration, Utc};
    use kapsel_core::storage::api_keys::ApiKey;

    let env = TestEnv::new_shared().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let mut tx = env.pool().begin().await.unwrap();

    let tenant1_id = env.create_tenant_tx(&mut tx, "api-tenant-1").await.unwrap();
    let tenant2_id = env.create_tenant_tx(&mut tx, "api-tenant-2").await.unwrap();

    let now = DateTime::<Utc>::from(env.clock.now_system());
    let future = now + chrono::Duration::hours(24);

    // Create API keys for tenant1
    let key1 = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "hash1".to_string(),
        tenant_id: tenant1_id,
        name: "Key 1".to_string(),
        created_at: now,
        expires_at: Some(future),
        revoked_at: None,
        last_used_at: None,
    };

    let key2 = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "hash2".to_string(),
        tenant_id: tenant1_id,
        name: "Key 2".to_string(),
        created_at: now,
        expires_at: Some(future),
        revoked_at: Some(now + Duration::seconds(1)), // This key is revoked
        last_used_at: None,
    };

    // Create API key for tenant2
    let key3 = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "hash3".to_string(),
        tenant_id: tenant2_id,
        name: "Key 3".to_string(),
        created_at: now,
        expires_at: Some(future),
        revoked_at: None,
        last_used_at: None,
    };

    storage.api_keys.create_in_tx(&mut tx, &key1).await.unwrap();
    storage.api_keys.create_in_tx(&mut tx, &key2).await.unwrap();
    storage.api_keys.create_in_tx(&mut tx, &key3).await.unwrap();

    // Test find_by_tenant (excluding revoked by default)
    let tenant1_keys =
        storage.api_keys.find_by_tenant_in_tx(&mut tx, tenant1_id, false).await.unwrap();
    assert_eq!(tenant1_keys.len(), 1);
    assert_eq!(tenant1_keys[0].key_hash, "hash1");

    // Test find_by_tenant (including revoked)
    let tenant1_all_keys =
        storage.api_keys.find_by_tenant_in_tx(&mut tx, tenant1_id, true).await.unwrap();
    assert_eq!(tenant1_all_keys.len(), 2);

    // Test tenant isolation
    let tenant2_keys =
        storage.api_keys.find_by_tenant_in_tx(&mut tx, tenant2_id, false).await.unwrap();
    assert_eq!(tenant2_keys.len(), 1);
    assert_eq!(tenant2_keys[0].key_hash, "hash3");

    // Test count_by_tenant (excluding revoked)
    let tenant1_count =
        storage.api_keys.count_by_tenant_in_tx(&mut tx, tenant1_id, false).await.unwrap();
    assert_eq!(tenant1_count, 1);

    // Test count_by_tenant (including revoked)
    let tenant1_all_count =
        storage.api_keys.count_by_tenant_in_tx(&mut tx, tenant1_id, true).await.unwrap();
    assert_eq!(tenant1_all_count, 2);

    // Test delete
    storage.api_keys.delete_in_tx(&mut tx, "hash1").await.unwrap();

    // Verify deletion
    let tenant1_keys_after =
        storage.api_keys.find_by_tenant_in_tx(&mut tx, tenant1_id, true).await.unwrap();
    assert_eq!(tenant1_keys_after.len(), 1); // Only revoked key remains
    assert_eq!(tenant1_keys_after[0].key_hash, "hash2");

    // Other tenant should be unaffected
    let tenant2_keys_after =
        storage.api_keys.find_by_tenant_in_tx(&mut tx, tenant2_id, false).await.unwrap();
    assert_eq!(tenant2_keys_after.len(), 1);
}

/// Tests cleanup of expired API keys.
///
/// This is essential for maintenance jobs to prevent key accumulation.
#[tokio::test]
async fn api_key_cleanup_expired() {
    use chrono::{Duration, Utc};
    use kapsel_core::storage::api_keys::ApiKey;

    let env = TestEnv::new_isolated().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let storage = Storage::new(env.pool().clone(), &clock);

    let suffix = Uuid::new_v4().simple().to_string();
    let tenant_id = env.create_tenant(&format!("cleanup-tenant-{}", suffix)).await.unwrap();

    let now = DateTime::<Utc>::from(env.clock.now_system());
    let past = now - chrono::Duration::hours(1);
    let future = now + chrono::Duration::hours(1);

    // Create keys with different expiration states
    let expired_key = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "expired".to_string(),
        tenant_id,
        name: "Expired Key".to_string(),
        created_at: past - Duration::hours(2), // Created before expiry
        expires_at: Some(past),                // Already expired
        revoked_at: None,
        last_used_at: None,
    };

    let valid_key = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "valid".to_string(),
        tenant_id,
        name: "Valid Key".to_string(),
        created_at: now,
        expires_at: Some(future), // Not expired
        revoked_at: None,
        last_used_at: None,
    };

    let no_expiry_key = ApiKey {
        id: Uuid::new_v4(),
        key_hash: "no_expiry".to_string(),
        tenant_id,
        name: "No Expiry Key".to_string(),
        created_at: now,
        expires_at: None, // Never expires
        revoked_at: None,
        last_used_at: None,
    };

    storage.api_keys.create(&expired_key).await.unwrap();
    storage.api_keys.create(&valid_key).await.unwrap();
    storage.api_keys.create(&no_expiry_key).await.unwrap();

    // Verify all keys exist
    assert_eq!(storage.api_keys.count_by_tenant(tenant_id, true).await.unwrap(), 3);

    // Cleanup expired keys
    let cleaned_count = storage.api_keys.cleanup_expired().await.unwrap();
    assert_eq!(cleaned_count, 1); // Only expired key should be removed

    // Verify only expired key was removed
    let remaining_keys = storage.api_keys.find_by_tenant(tenant_id, true).await.unwrap();
    assert_eq!(remaining_keys.len(), 2);

    let remaining_hashes: Vec<_> = remaining_keys.iter().map(|k| &k.key_hash).collect();
    assert!(remaining_hashes.contains(&&"valid".to_string()));
    assert!(remaining_hashes.contains(&&"no_expiry".to_string()));
    assert!(!remaining_hashes.contains(&&"expired".to_string()));
}
