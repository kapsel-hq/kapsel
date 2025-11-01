# Testing Strategy

Comprehensive testing methodology for proving webhook reliability correctness through deterministic simulation and property-based validation.

**Related Documents:**

- [Architecture](ARCHITECTURE.md) - System design supporting testability
- [Current Status](STATUS.md) - Testing coverage status
- [Technical Specification](SPECIFICATION.md) - Requirements validated by tests

## Philosophy

The test suite IS the system specification. Every test represents an invariant that must hold true for webhook reliability. We don't test to find bugs—we test to prove correctness through exhaustive validation.

## The Testing Pyramid

```
        ┌──────────────┐
        │   Chaos      │ 1%   - Production-like failure injection
        ├──────────────┤
        │  End-to-End  │ 2%   - Full system flows with real dependencies
        ├──────────────┤
        │  Scenario    │ 10%  - Multi-step workflows with time control
        ├──────────────┤
        │ Integration  │ 31%  - Component boundaries with real database
        ├──────────────┤
        │  Property    │ 14%  - Invariant validation with generated inputs
        ├──────────────┤
        │    Unit      │ 42%  - Pure logic, no I/O
        └──────────────┘
```

**Current Status:** 292 tests across all layers (124 unit, 168 integration/property/e2e/chaos)

This inverted pyramid reflects webhook reliability priorities: complex interactions matter more than isolated functions.

## Test Layers

### Unit Tests (42% - 124 tests)

**Purpose:** Validate pure business logic. Zero I/O, zero dependencies.

**Tools:** Standard `#[test]`, no external crates needed.

**Location:** Inline with implementation in `crates/*/src/` using `#[cfg(test)] mod tests`.

**What we test:**

Retry logic:

- Exponential backoff timing with jitter
- Maximum retry attempt bounds
- Error classification (retryable vs non-retryable)

Circuit breaker state machine:

- Initial state (closed)
- Failure threshold triggers open state
- Half-open recovery with probe requests
- Success/failure transitions from half-open

Cryptographic operations:

- HMAC signature validation
- Provider-specific signature formats (Stripe, GitHub, etc.)
- Ed25519 signing and verification

### Property Tests (14% - ~40 tests)

**Purpose:** Verify invariants hold for all inputs. Find edge cases humans miss.

**Tools:** `proptest` with custom strategies for domain types.

**Location:** Crate-level `crates/*/tests/property_test.rs` files for domain-specific invariants, workspace-level `tests/property_test.rs` for cross-system invariants.

**What we test:**

System-wide invariants:

- Idempotency: duplicate webhooks always return same event ID
- Bounded retries: never exceed configured max_retries
- Exponential growth: backoff delays increase correctly with jitter
- Circuit breaker transitions: state machine never enters invalid state
- No data loss: event accounting preserved across failures
- Tenant isolation: cross-tenant data never leaks

Domain-specific invariants:

- Merkle tree properties: inclusion proofs always verify for included leaves
- Signature verification: valid signatures pass, tampered signatures fail
- Delivery ordering: events delivered in correct order for same endpoint

### Integration Tests (31% - ~90 tests)

**Purpose:** Verify component boundaries and database interactions.

**Tools:** `kapsel-testing` with real PostgreSQL, transaction isolation.

**Location:** `crates/*/tests/*.rs` for crate-level integration.

**What we test:**

Attestation integration:

- Successful delivery creates Merkle tree leaf
- Failed delivery does not create attestation
- Batch commitment includes all pending leaves
- Signed tree head generation and verification

Delivery worker behavior:

- Workers claim pending events using SKIP LOCKED
- HTTP client handles retries with backoff
- Delivery engine lifecycle (startup, graceful shutdown)
- Concurrent deliveries to different endpoints

API integration:

- Webhook ingestion with authentication
- API key validation and tenant resolution
- Health check endpoints with database connectivity
- Error response formatting

Database state persistence:

- Circuit breaker state survives service restart
- Pending events survive crashes
- Delivery order matches ingestion order

### Scenario Tests (10% - ~30 tests)

**Purpose:** Multi-step workflows with deterministic time control.

**Tools:** `ScenarioBuilder`, `TestClock`, `MockServer`.

**Location:** Embedded in `crates/*/tests/*_test.rs` files.

**What we test:**

Retry workflows:

- Complete retry sequence with exponential backoff
- Failed attempts advance time correctly
- Success after N retries completes workflow
- Total elapsed time matches expected backoff schedule

Circuit breaker cascade prevention:

- Multiple failures trigger circuit open
- Subsequent requests fail fast without HTTP attempts
- Half-open recovery with probe requests
- Success transitions back to closed state

Rate limiting respect:

- HTTP 429 responses schedule retry based on Retry-After header
- No delivery attempts before retry window expires
- Successful delivery after rate limit window passes

### End-to-End Tests (2% - ~5 tests)

**Purpose:** Validate complete system flows with real HTTP server.

**Tools:** Full Axum server, real PostgreSQL, `MockServer` for destinations.

**Location:** `tests/e2e_test.rs` in workspace root.

**What we test:**

Complete webhook journey:

- HTTP POST to ingestion endpoint returns 200
- Webhook delivered to destination with correct headers
- Delivery attempts recorded in audit trail
- Response status and timing captured correctly

Multi-tenant isolation:

- Tenant A cannot access Tenant B's events via HTTP
- HTTP 404 returned (not 403) to prevent information leakage
- Database queries automatically filter by tenant_id
- API key authentication resolves to correct tenant

System health:

- Health check endpoint reports database connectivity
- Service starts up and accepts requests
- Graceful shutdown completes in-flight deliveries

### Chaos Tests (1% - ~3 tests)

**Purpose:** Validate system resilience under failure.

**Tools:** Custom chaos injection via mocked HTTP failures.

**Location:** `tests/chaos_test.rs` in workspace root.

**What we test:**

Intermittent network failures:

- Random HTTP failures during delivery
- System eventually delivers all webhooks
- Retry logic handles transient failures
- No data loss despite unreliable network

Permanent endpoint failures:

- Endpoint that never succeeds
- Circuit breaker opens after threshold
- Subsequent webhooks fail fast
- System remains responsive

Database connection loss:

- Temporary database unavailability
- Worker pool handles connection errors
- Delivery resumes after reconnection
- Event state consistency maintained

## The Golden Sample Test

This test embodies our entire philosophy: deterministic, complex, invariant-focused.

```rust
#[tokio::test]
async fn golden_webhook_delivery_with_retry_backoff() {
    // Setup: Deterministic environment
    let env = TestEnv::new_shared().await?;
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

Performance benchmarks are not yet implemented. Target metrics for future benchmarking:

- **Ingestion latency p99**: < 50ms
- **Delivery worker throughput**: > 1000 webhooks/sec
- **Database query p99**: < 5ms
- **Memory per 1K webhooks**: < 200MB
- **CPU per 1K webhooks/sec**: < 1 core

## Test Organization

The test suite is now organized using a structured pyramid approach:

```
tests/                              # Workspace E2E Tests
├── property_tests.rs              # Cross-system invariants
├── chaos_test.rs                  # System resilience
├── golden_sample_test.rs          # End-to-end scenarios
└── health_check_test.rs           # Service health

crates/*/tests/                     # Crate-Level Tests
├── property_test.rs               # Domain invariants
├── integration_test.rs            # Component integration
├── scenarios.rs                   # Multi-step workflows
└── *_test.rs                      # Specific test suites

crates/*/src/                       # Unit Tests
└── lib.rs (with #[cfg(test)])     # Inline with implementation
```

This organization provides:

- **Fast feedback**: Run crate-specific tests during development
- **Clear ownership**: Each crate owns its test scenarios
- **Parallel execution**: Test crates independently
- **Proper isolation**: E2E tests verify cross-system behavior

## Test Maintenance

- **Parallel execution**: Tests run in isolated transactions
- **Test runtime budget**: < 5 min for unit+integration
- **Coverage target**: 80% minimum, 100% for critical paths

## Status

**Future priorities:**

- Benchmark suite with regression detection
- Performance test harness for sustained load
- CI test sharding for faster builds

## The Standard

Every test must:

1. Be deterministic (same result every run)
2. Test one behavior (single assertion focus)
3. Use descriptive names (behavior, not implementation)
4. Run in isolation (no test dependencies)
5. Complete quickly (< 100ms for unit, < 500ms for integration)
