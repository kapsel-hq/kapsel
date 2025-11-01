//! Tests for ScenarioBuilder functionality.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::Result;
use kapsel_core::EventStatus;
use kapsel_testing::{fixtures::WebhookBuilder, http::MockResponse, ScenarioBuilder, TestEnv};

#[tokio::test]
async fn scenario_builder_executes_steps() -> Result<()> {
    let mut env = TestEnv::new_shared().await?;

    let scenario = ScenarioBuilder::new("test scenario")
        .advance_time(Duration::from_secs(1))
        .assert_state(|env| {
            // Custom assertion
            assert!(env.elapsed() >= Duration::from_secs(1));
            Ok(())
        })
        .advance_time(Duration::from_secs(2))
        .assert_state(|env| {
            // Verify cumulative time
            assert!(env.elapsed() >= Duration::from_secs(3));
            Ok(())
        })
        .check_invariant(|_env| {
            Box::pin(async move {
                // Invariant check runs after each step
                Ok(())
            })
        });

    scenario.run(&mut env).await?;
    Ok(())
}

#[tokio::test]
async fn scenario_builder_invariant_checks_execute() -> Result<()> {
    let mut env = TestEnv::new_shared().await?;

    // Test that invariant checks are actually executed
    let check_executed = Arc::new(AtomicBool::new(false));
    let check_executed_clone = check_executed.clone();

    let scenario = ScenarioBuilder::new("invariant test scenario")
        .advance_time(Duration::from_secs(1))
        .check_invariant(move |_env| {
            let check_ref = check_executed_clone.clone();
            Box::pin(async move {
                check_ref.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

    scenario.run(&mut env).await?;

    // Verify the invariant check was actually executed
    assert!(check_executed.load(Ordering::SeqCst), "Invariant check should have been executed");

    Ok(())
}

#[tokio::test]
async fn scenario_builder_invariant_failure_caught() -> Result<()> {
    let mut env = TestEnv::new_shared().await?;

    let scenario = ScenarioBuilder::new("failing invariant scenario")
        .advance_time(Duration::from_secs(1))
        .check_invariant(|_env| Box::pin(async move { anyhow::bail!("Test invariant violation") }));

    let result = scenario.run(&mut env).await;
    assert!(result.is_err(), "Scenario should fail due to invariant violation");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("invariant check"),
        "Error should mention invariant failure: {error_msg}"
    );

    Ok(())
}

#[tokio::test]
async fn scenario_builder_snapshot_integration() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("scenario-snapshot").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Setup HTTP mock for success
        env.http_mock
            .mock_simple("/", MockResponse::Success {
                status: reqwest::StatusCode::OK,
                body: bytes::Bytes::from_static(b"OK"),
            })
            .await;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("scenario-snap")
            .body(b"scenario integration test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Verify data exists
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(env.pool())
            .await?;
        assert_eq!(count, 1, "Event should exist");

        // Test delivery
        env.run_delivery_cycle().await?;

        // Verify final state
        let status = env.find_webhook_status(event_id).await?;
        assert_eq!(status, EventStatus::Delivered, "Event should be delivered");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn scenario_builder_snapshot_integration_comprehensive() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("scenario-comprehensive").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Setup HTTP mock for success
        env.http_mock
            .mock_simple("/", MockResponse::Success {
                status: reqwest::StatusCode::OK,
                body: bytes::Bytes::from_static(b"OK"),
            })
            .await;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("scenario-integration")
            .body(b"scenario integration test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Verify data exists
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE id = $1")
            .bind(event_id.0)
            .fetch_one(env.pool())
            .await?;
        assert_eq!(count, 1, "Event should exist");

        // Test delivery
        env.run_delivery_cycle().await?;

        // Verify final state
        let events = env.get_all_events().await?;
        assert_eq!(events.len(), 1, "Should have processed exactly one webhook");
        assert_eq!(events[0].status, EventStatus::Delivered, "Webhook should be delivered");
        assert_eq!(events[0].attempt_count(), 1, "Should have one attempt");

        Ok(())
    })
    .await
}

#[tokio::test]
async fn scenario_builder_with_retries() -> Result<()> {
    TestEnv::run_isolated_test(|mut env| async move {
        let tenant_id = env.create_tenant("retry-scenario").await?;
        let endpoint_id = env.create_endpoint(tenant_id, &env.http_mock.url()).await?;

        // Setup HTTP responses: fail -> succeed pattern
        env.http_mock
            .mock_sequence()
            .respond_with(503, "Service Unavailable")
            .respond_with(200, "OK")
            .build()
            .await;

        let webhook = WebhookBuilder::new()
            .tenant(tenant_id.0)
            .endpoint(endpoint_id.0)
            .source_event("retry-test")
            .body(b"retry test payload".to_vec())
            .build();

        let event_id = env.ingest_webhook(&webhook).await?;

        // Build scenario with retry checks
        let scenario = ScenarioBuilder::new("retry scenario")
            .check_retry_bounds(5)
            .run_delivery_cycle()
            .advance_time(Duration::from_secs(5))
            .run_delivery_cycle()
            .expect_status(event_id, EventStatus::Delivered)
            .expect_delivery_attempts(event_id, 2);

        scenario.run(&mut env).await?;
        Ok(())
    })
    .await
}
