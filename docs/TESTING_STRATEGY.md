# Testing Strategy

Comprehensive testing methodology for proving webhook reliability correctness through deterministic simulation and property-based validation.

**Related Documents:**

- [System Overview](OVERVIEW.md) - Architecture supporting testability by design
- [Implementation Status](IMPLEMENTATION_STATUS.md) - Current testing coverage status
- [Technical Specification](SPECIFICATION.md) - Requirements validated by test suite

## Philosophy

The test suite IS the system specification. Every test represents an invariant that must hold true for webhook reliability. We don't test to find bugs—we test to prove correctness through exhaustive validation.

## The Testing Pyramid

```
        ┌──────────────┐
        │   Chaos      │ 1%   - Production-like failure injection
        ├──────────────┤
        │  End-to-End  │ 5%   - Full system flows with real dependencies
        ├──────────────┤
        │  Scenario    │ 15%  - Multi-step workflows with time control
        ├──────────────┤
        │ Integration  │ 25%  - Component boundaries with real database
        ├──────────────┤
        │  Property    │ 25%  - Invariant validation with generated inputs
        ├──────────────┤
        │    Unit      │ 29%  - Pure logic, no I/O
        └──────────────┘
```

This inverted pyramid reflects webhook reliability priorities: complex interactions matter more than isolated functions.

## Test Layers

### Unit Tests (29%)

**Purpose:** Validate pure business logic. Zero I/O, zero dependencies.

**Tools:** Standard `#[test]`, no external crates needed.

**Location:** Inline with implementation in `crates/*/src/`.

**Examples:**

```rust
// Exponential backoff calculation correctness
#[test]
fn backoff_doubles_with_jitter() {
    let delay1 = calculate_backoff(0);
    let delay2 = calculate_backoff(1);
    assert!(delay2 >= delay1 * 1.5 && delay2 <= delay1 * 2.5);
}

// Circuit breaker state transitions
#[test]
fn circuit_opens_after_threshold() {
    let mut circuit = CircuitBreaker::new(5, 0.5);
    for _ in 0..5 {
        circuit.record_failure();
    }
    assert_eq!(circuit.state(), CircuitState::Open);
}

// Idempotency key extraction
#[test]
fn extracts_stripe_event_id() {
    let payload = json!({"id": "evt_123"});
    let key = extract_idempotency_key(&payload, Strategy::StripeId);
    assert_eq!(key, "evt_123");
}
```

### Property Tests (25%)

**Purpose:** Verify invariants hold for all inputs. Find edge cases humans miss.

**Tools:** `proptest` with custom strategies for domain types.

**Location:** Separate module in `crates/*/src/` files.

**Examples:**

```rust
proptest! {
    // Idempotent operations remain idempotent
    #[test]
    fn duplicate_webhooks_return_same_response(
        webhook in webhook_strategy(),
        jitter in 0u64..1000
    ) {
        let response1 = process_webhook(&webhook);
        std::thread::sleep(Duration::from_millis(jitter));
        let response2 = process_webhook(&webhook);
        prop_assert_eq!(response1, response2);
    }

    // Retry count never exceeds maximum
    #[test]
    fn retry_count_bounded(
        failures in 0usize..100
    ) {
        let attempts = simulate_retries(failures);
        prop_assert!(attempts <= MAX_RETRIES);
    }

    // Total retry time respects bounds
    #[test]
    fn total_backoff_time_bounded(
        attempt_count in 1usize..20
    ) {
        let total: Duration = (0..attempt_count)
            .map(calculate_backoff)
            .sum();
        prop_assert!(total <= MAX_TOTAL_RETRY_TIME);
    }
}
```

### Integration Tests (25%)

**Purpose:** Verify component boundaries and database interactions.

**Tools:** `kapsel-testing` with real PostgreSQL, transaction isolation.

**Location:** `tests/` directory for cross-crate tests.

**Examples:**

```rust
// Database constraint enforcement
#[tokio::test]
async fn unique_constraint_prevents_duplicates() {
    let env = TestEnv::new().await?;
    let webhook = WebhookBuilder::with_defaults().build();

    env.insert_webhook(&webhook).await?;
    let result = env.insert_webhook(&webhook).await;

    assert!(matches!(result, Err(Error::DuplicateEvent)));
}

// Concurrent webhook processing
#[tokio::test]
async fn concurrent_deliveries_maintain_order() {
    let env = TestEnv::new().await?;
    let webhooks: Vec<_> = (0..100)
        .map(|i| WebhookBuilder::with_sequence(i))
        .collect();

    // Process concurrently
    let handles: Vec<_> = webhooks
        .iter()
        .map(|w| env.process_webhook(w.clone()))
        .collect();

    futures::future::join_all(handles).await;

    // Verify delivery order matches ingestion order
    let delivered = env.get_delivered_order().await?;
    assert_eq!(delivered, (0..100).collect::<Vec<_>>());
}

// Circuit breaker database persistence
#[tokio::test]
async fn circuit_state_survives_restart() {
    let env = TestEnv::new().await?;
    let endpoint_id = env.create_endpoint().await?;

    // Open circuit
    for _ in 0..5 {
        env.record_failure(endpoint_id).await?;
    }

    // Simulate restart by creating new env with same DB
    let env2 = TestEnv::with_database(env.db_url()).await?;
    let state = env2.get_circuit_state(endpoint_id).await?;

    assert_eq!(state, CircuitState::Open);
}
```

### Scenario Tests (15%)

**Purpose:** Multi-step workflows with deterministic time control.

**Tools:** `ScenarioBuilder`, `TestClock`, `MockServer`.

**Location:** `tests/scenarios/` directory.

**Examples:**

```rust
// Complete retry with backoff scenario
#[tokio::test]
async fn webhook_retries_with_exponential_backoff() {
    let env = TestEnv::new().await?;

    ScenarioBuilder::new("exponential backoff")
        .ingest("endpoint_1", stripe_webhook())
        .inject_failure(FailureKind::Http500)
        .expect_retry_scheduled(Duration::from_secs(1))
        .advance_time(Duration::from_secs(1))
        .inject_failure(FailureKind::Http500)
        .expect_retry_scheduled(Duration::from_secs(2))
        .advance_time(Duration::from_secs(2))
        .inject_success()
        .expect_delivered()
        .assert_total_time(Duration::from_secs(3))
        .run(&env)
        .await?;
}

// Circuit breaker prevents cascade failure
#[tokio::test]
async fn circuit_breaker_stops_cascade() {
    let env = TestEnv::new().await?;

    ScenarioBuilder::new("circuit breaker")
        .create_webhooks(10)
        .inject_failure(FailureKind::NetworkTimeout)
        .deliver_batch(5) // First 5 fail
        .expect_circuit_open()
        .deliver_batch(5) // Next 5 fail fast
        .assert_attempts(|attempts| {
            // First 5 made real attempts
            assert_eq!(attempts[0..5].iter().sum(), 5);
            // Last 5 failed immediately
            assert_eq!(attempts[5..10].iter().sum(), 0);
        })
        .run(&env)
        .await?;
}

// Rate limit handling with Retry-After
#[tokio::test]
async fn respects_rate_limit_headers() {
    let env = TestEnv::new().await?;

    ScenarioBuilder::new("rate limit respect")
        .ingest("endpoint_1", github_webhook())
        .inject_failure(FailureKind::Http429 {
            retry_after: Duration::from_secs(60)
        })
        .expect_no_retry_before(Duration::from_secs(60))
        .advance_time(Duration::from_secs(59))
        .assert_no_delivery_attempt()
        .advance_time(Duration::from_secs(2))
        .expect_delivery()
        .run(&env)
        .await?;
}
```

### End-to-End Tests (5%)

**Purpose:** Validate complete system flows with real HTTP server.

**Tools:** Full Axum server, real PostgreSQL, `MockServer` for destinations.

**Location:** `tests/e2e/` directory.

**Examples:**

```rust
// Complete webhook journey
#[tokio::test]
async fn webhook_journey_from_ingestion_to_delivery() {
    let env = TestEnv::with_server().await?;
    let destination = MockServer::start().await;

    // Create endpoint pointing to mock
    let endpoint = env.create_endpoint()
        .url(destination.url())
        .build()
        .await?;

    // Configure destination to succeed
    destination.expect_webhook()
        .with_header("X-Kapsel-Event-Id", Any)
        .respond_ok()
        .await;

    // Ingest webhook
    let response = env.client
        .post(format!("{}/ingest/{}", env.url(), endpoint.id))
        .json(&stripe_webhook())
        .send()
        .await?;

    assert_eq!(response.status(), 200);

    // Verify delivery
    destination.wait_for_request(Duration::from_secs(5)).await?;

    // Check audit trail
    let event_id = response.json::<Value>().await?["event_id"].as_str().unwrap();
    let attempts = env.get_delivery_attempts(event_id).await?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].response_status, 200);
}

// Multi-tenant isolation
#[tokio::test]
async fn tenants_completely_isolated() {
    let env = TestEnv::with_server().await?;

    let tenant_a = env.create_tenant("A").await?;
    let tenant_b = env.create_tenant("B").await?;

    // Ingest webhooks for both tenants
    let event_a = env.ingest_for_tenant(tenant_a, webhook()).await?;
    let event_b = env.ingest_for_tenant(tenant_b, webhook()).await?;

    // Tenant A cannot access Tenant B's events
    let result = env.client
        .get(format!("{}/v1/events/{}", env.url(), event_b))
        .bearer_auth(tenant_a.api_key)
        .send()
        .await?;

    assert_eq!(result.status(), 404);
}
```

### Chaos Tests (1%)

**Purpose:** Validate system resilience under failure.

**Tools:** Custom chaos injection, `fail` points.

**Location:** `tests/chaos/` directory, CI-only.

**Examples:**

```rust
// Random network failures during delivery
#[tokio::test]
#[ignore] // Only in CI
async fn survives_random_network_failures() {
    let env = TestEnv::new().await?;
    let chaos = ChaosInjector::new()
        .network_failure_rate(0.3)
        .database_failure_rate(0.1);

    // Ingest 1000 webhooks
    let webhooks = (0..1000)
        .map(|_| random_webhook())
        .collect::<Vec<_>>();

    for webhook in &webhooks {
        env.ingest(webhook).await?;
    }

    // Process with chaos
    chaos.run_for(Duration::from_secs(60), || async {
        env.process_pending().await
    }).await;

    // All webhooks eventually delivered
    for webhook in webhooks {
        assert!(env.is_delivered(webhook.id).await?);
    }
}
```

## The Golden Sample Test

This test embodies our entire philosophy: deterministic, complex, invariant-focused.

```rust
#[tokio::test]
async fn golden_webhook_delivery_with_retry_backoff() {
    // Setup: Deterministic environment
    let env = TestEnv::new().await?;
    let destination = MockServer::start().await;

    // Create endpoint with specific retry policy
    let endpoint = env.create_endpoint()
        .url(destination.url())
        .max_retries(10)
        .timeout(Duration::from_secs(30))
        .build()
        .await?;

    // Configure destination to fail 3 times, then succeed
    destination
        .expect(3).respond(503)
        .then_expect(1).respond(200)
        .build()
        .await;

    // Ingest webhook at T=0
    let start_time = env.clock.now();
    let webhook = WebhookBuilder::stripe()
        .idempotency_key("payment_123")
        .build();

    let event_id = env.ingest(endpoint.id, webhook).await?;

    // Process with deterministic time control
    let expected_delays = vec![
        Duration::from_secs(1),   // First retry after 1s
        Duration::from_secs(2),   // Second retry after 2s
        Duration::from_secs(4),   // Third retry after 4s
    ];

    for (attempt, expected_delay) in expected_delays.iter().enumerate() {
        // Advance time to next retry
        env.clock.advance(*expected_delay);

        // Trigger delivery attempt
        env.process_pending().await?;

        // Verify attempt was made
        let attempts = env.get_attempts(event_id).await?;
        assert_eq!(attempts.len(), attempt + 2); // Initial + retries

        // Verify timing
        let actual_delay = attempts[attempt + 1].attempted_at
            - attempts[attempt].attempted_at;
        assert_eq!(actual_delay, *expected_delay);
    }

    // Final successful delivery
    let status = env.get_event_status(event_id).await?;
    assert_eq!(status, EventStatus::Delivered);

    // Verify invariants
    let attempts = env.get_attempts(event_id).await?;
    assert_eq!(attempts.len(), 4); // 1 initial + 3 retries
    assert_eq!(attempts[0..3].iter().all(|a| a.response_status == 503), true);
    assert_eq!(attempts[3].response_status, 200);

    // Verify total elapsed time
    let total_time = env.clock.now() - start_time;
    let expected_total = Duration::from_secs(7); // 1 + 2 + 4
    assert_eq!(total_time, expected_total);

    // Verify idempotency - resending same webhook returns same response
    let duplicate_id = env.ingest(endpoint.id, webhook).await?;
    assert_eq!(event_id, duplicate_id);

    // No additional attempts were made
    let attempts_after = env.get_attempts(event_id).await?;
    assert_eq!(attempts_after.len(), 4);
}
```

## Performance Testing

### Benchmarks

Located in `benches/`, run with `cargo bench`.

```rust
// Ingestion throughput
fn bench_ingestion_throughput(c: &mut Criterion) {
    c.bench_function("ingest_10k_webhooks", |b| {
        b.iter_custom(|iters| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();
                let start = Instant::now();

                for _ in 0..iters {
                    for _ in 0..10_000 {
                        env.ingest(random_webhook()).await.unwrap();
                    }
                }

                start.elapsed()
            })
        });
    });
}
```

### Load Tests

```rust
// Sustained load test
#[tokio::test]
#[ignore] // Manual execution
async fn sustained_10k_webhooks_per_second() {
    let env = TestEnv::with_production_config().await?;
    let metrics = MetricsCollector::new();

    // Run for 1 hour
    let duration = Duration::from_secs(3600);
    let start = Instant::now();

    while start.elapsed() < duration {
        let batch_start = Instant::now();

        // Send 10k webhooks
        let handles: Vec<_> = (0..10_000)
            .map(|_| {
                let env = env.clone();
                tokio::spawn(async move {
                    env.ingest(random_webhook()).await
                })
            })
            .collect();

        let results = futures::future::join_all(handles).await;

        // Record metrics
        metrics.record_batch(batch_start.elapsed(), results);

        // Maintain rate
        if batch_start.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_secs(1) - batch_start.elapsed()).await;
        }
    }

    // Assert performance targets
    assert!(metrics.p99_latency() < Duration::from_millis(50));
    assert!(metrics.success_rate() > 0.999);
    assert!(metrics.memory_usage() < 2_000_000_000); // 2GB
}
```

### KPIs to Track

- **Ingestion latency p99**: < 50ms
- **Delivery worker throughput**: > 1000 webhooks/sec
- **Database query p99**: < 5ms
- **Memory per 1K webhooks**: < 200MB
- **CPU per 1K webhooks/sec**: < 1 core

## Developer Workflow

### TDD Cycle

1. **RED**: Write failing test first

```rust
#[tokio::test]
async fn new_circuit_breaker_strategy() {
    let env = TestEnv::new().await?;
    let circuit = AdaptiveCircuitBreaker::new();

    // This will fail - implement to make green
    assert_eq!(circuit.calculate_threshold(window), expected);
}
```

2. **GREEN**: Minimal implementation

```rust
impl AdaptiveCircuitBreaker {
    fn calculate_threshold(&self, window: &Window) -> usize {
        // Simplest thing that makes test pass
        5
    }
}
```

3. **REFACTOR**: Improve with tests as safety net

```rust
impl AdaptiveCircuitBreaker {
    fn calculate_threshold(&self, window: &Window) -> usize {
        // Proper implementation with confidence
        let error_rate = window.failures() / window.total();
        if error_rate > 0.5 {
            (window.total() * 0.1).max(1)
        } else {
            self.base_threshold
        }
    }
}
```

### Adding a Feature

Example: Implementing webhook replay

1. Start with integration test:

```rust
#[tokio::test]
async fn replay_webhook_creates_new_delivery() {
    let env = TestEnv::new().await?;
    let event_id = env.ingest(webhook()).await?;
    env.deliver(event_id).await?;

    // Replay
    let replay_id = env.replay(event_id).await?;

    assert_ne!(event_id, replay_id);
    assert_eq!(env.get_status(replay_id).await?, EventStatus::Pending);
}
```

2. Add unit tests for components:

```rust
#[test]
fn replay_preserves_original_headers() {
    let original = webhook();
    let replayed = original.create_replay();
    assert_eq!(original.headers, replayed.headers);
}
```

3. Property test invariants:

```rust
proptest! {
    #[test]
    fn replay_maintains_idempotency(webhook in webhook_strategy()) {
        let replay1 = webhook.create_replay();
        let replay2 = webhook.create_replay();
        prop_assert_eq!(replay1.idempotency_key, replay2.idempotency_key);
    }
}
```

## Long-Term Evolution

### Keeping Tests Fast

- **Parallel execution**: Tests run in isolated transactions
- **Shared fixtures**: Reuse expensive setup across tests
- **Test sharding**: Split by module in CI
- **Smart reruns**: Only run affected tests on changes

### Advanced Techniques

**Deterministic Simulation Testing**

```rust
// Coming: Full system simulation
SimulationBuilder::new()
    .with_seed(12345) // Reproducible
    .add_clients(1000)
    .add_endpoints(100)
    .failure_rate(0.1)
    .run_for(Duration::from_hours(24))
    .assert_invariants()
    .await?;
```

**Production Testing**

```rust
// Shadow traffic replay
ProductionTest::new()
    .capture_traffic(Duration::from_hours(1))
    .replay_against(staging_env)
    .compare_responses()
    .await?;
```

**Formal Verification**

```rust
// Model checking with TLA+ specs
#[formal_spec("webhook_delivery.tla")]
fn delivery_guarantees_at_least_once() {
    // Spec verified at compile time
}
```

### Test Maintenance

- **Quarterly test audit**: Remove redundant tests
- **Flaky test budget**: Max 1% flaky rate
- **Test runtime budget**: < 5 min for unit+integration
- **Coverage target**: 80% minimum, 100% for critical paths

## Implementation Checklist

Immediate priorities for test infrastructure:

- [ ] Implement `TestClock` in all async operations
- [ ] Add property test strategies for all domain types
- [ ] Create chaos injection framework
- [ ] Build webhook scenario DSL
- [ ] Set up benchmark suite with regression detection
- [ ] Add test fixtures for all provider types (Stripe, GitHub, etc.)
- [ ] Implement deterministic mock HTTP server
- [ ] Create invariant assertion library
- [ ] Build performance test harness
- [ ] Set up CI test sharding

## The Standard

Every test must:

1. Be deterministic (same result every run)
2. Test one behavior (single assertion focus)
3. Use descriptive names (behavior, not implementation)
4. Run in isolation (no test dependencies)
5. Complete quickly (< 100ms for unit, < 500ms for integration)

No merge without tests. No exception.
