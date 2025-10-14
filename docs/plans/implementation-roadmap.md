# Kapsel Implementation Roadmap

## Current State Assessment

### Ground Truth vs Documentation

After comprehensive codebase review, here's the honest state:

**What Works:**

- Webhook ingestion endpoint (`/ingest/:endpoint_id`)
- PostgreSQL persistence with idempotency
- Test infrastructure (TestEnv, MockServer, TestClock)
- Domain models and error taxonomy
- Basic worker pool structure
- HTTP delivery client (COMPLETE - confirmed via TDD tests)
- Delivery attempt persistence (COMPLETE - implemented)

**What's Missing:**

- Retry logic with exponential backoff
- Circuit breaker integration
- Performance benchmarks (using mock data)
- Management API endpoints
- Metrics and monitoring

**Technical Debt:**

- Mock memory_usage() in benchmarks
- Database connection pool exhaustion during parallel tests

### Test Coverage Analysis

Current test distribution:

- Unit tests: ~20 tests (crate-level)
- Integration tests: ~35 tests
- Property tests: ~5 test properties
- End-to-end tests: ~7 tests

**Coverage Gaps:**

- No delivery client tests (RESOLVED: comprehensive TDD suite added)
- No circuit breaker integration tests
- No load/stress tests
- No chaos/failure injection tests
- ScenarioBuilder completely unused despite powerful capabilities

## Comprehensive Testing Assessment

### Executive Summary

The testing infrastructure is exceptionally powerful and well-designed, reflecting a deep commitment to correctness and reliability. The existing tests, particularly the property tests and the "golden sample" test, demonstrate an advanced understanding of modern testing principles.

However, the tests are **not yet using the full potential of the harness**. Several key features designed to improve test readability and expand coverage are currently underutilized or not used at all. While the foundation is world-class, the application of this foundation is inconsistent.

**Overall State:** The testing culture is strong, and the foundation is excellent. The primary opportunity is to apply the harness's full capabilities more broadly to enhance readability, reduce boilerplate, and cover more complex failure scenarios.

### Strengths: What Tests Are Doing Well

1. **Property-Based Testing is Top-Tier:** The use of `proptest` in `tests/property_tests.rs` is exemplary.
   - Exhaustively validates critical invariants like idempotency, retry bounds, and circuit breaker logic
   - Includes fuzz tests that probe for edge cases in retry calculations and error handling
   - This proactive approach to finding bugs in business logic is a sign of mature testing strategy

2. **Deterministic Simulation is Masterful:** The `golden_sample_test.rs` is a showcase of deterministic testing.
   - Simulates complex multi-step "fail-then-succeed" scenarios by manipulating database and using `TestClock`
   - Allows exact verification of time-based logic like exponential backoff without flakiness
   - This is the "gold standard" for testing reliability features

3. **Database Isolation is Production-Grade:** The harness's `TestDatabase` provides transaction-based isolation used effectively in integration tests, ensuring tests are fast, independent, and don't conflict

4. **Comprehensive CI Pipeline:** GitHub Actions workflow covers formatting, linting, security audits, dependency checks, and multi-OS/multi-version test matrix

### Critical Opportunities for Improvement

1. **The `ScenarioBuilder` is Completely Unused:**
   - The test harness provides a powerful, declarative `ScenarioBuilder` for defining multi-step integration tests
   - Currently, NO tests use this feature. The `golden_sample_test.rs` implements complex scenarios manually
   - Refactoring to use `ScenarioBuilder` would make tests dramatically more readable and maintainable

2. **Chaos Testing is Mentioned but Not Implemented:**
   - Project documentation highlights chaos testing as a key capability
   - However, there are NO actual chaos tests in the project. This is major untapped potential for testing system resilience to production-like failures in a controlled, deterministic way

3. **Inconsistent Test Styles:**
   - Two distinct styles: Full Server Tests vs Simulated Tests
   - The simulated approach (like golden sample test) is more aligned with harness philosophy and generally faster/less flaky
   - Project would benefit from favoring simulated style for most integration tests

4. **Test Helpers are Underdeveloped:**
   - Tests frequently use raw `sqlx::query()` calls to set up data
   - While `TestEnv` has helpers like `create_tenant`, it lacks helpers for common actions like ingesting webhooks directly
   - Expanding helpers would reduce boilerplate and make tests more resilient to schema changes

### Actionable Testing Improvements

1. **Refactor Golden Sample Test:** Convert to use `ScenarioBuilder` as practical example
2. **Implement First Chaos Test:** Create `tests/chaos_delivery_test.rs` using `ScenarioBuilder`
3. **Expand `TestEnv` Helpers:** Add `ingest_webhook_for_test()` and similar functions
4. **Prioritize Simulated Integration Tests:** Use deterministic simulation over full server tests
5. **Complete Draft Tests:** The `delivery_client_test.rs` needs completion for HTTP client behavior

## Implementation Strategy

### Phase 1: Testing Infrastructure Enhancement & Core Reliability (Week 1)

**Objective:** Modernize test suite to use harness capabilities and complete webhook reliability

**Status Update:** HTTP delivery client is COMPLETE and fully functional. However, testing infrastructure needs improvement to match the quality of the harness.

**Critical Testing Work (Days 1-2):**

1. **Refactor Golden Sample Test to ScenarioBuilder:**

   ```rust
   // Before: Manual database manipulation
   sqlx::query("INSERT ...").await?;
   env.clock.advance(*expected_delay);

   // After: Declarative scenario
   ScenarioBuilder::new("golden webhook delivery")
       .ingest(webhook)
       .expect_failure(FailureKind::Http503)
       .advance_time(Duration::from_secs(1))
       .expect_success()
       .run(&env).await?;
   ```

2. **Implement First Chaos Test:**

   ```rust
   #[tokio::test]
   async fn chaos_database_disconnection_during_delivery() {
       ScenarioBuilder::new("database failure recovery")
           .ingest_batch(100)
           .start_delivery()
           .inject_failure(ChaosFailure::DatabaseDisconnect { duration: Duration::from_secs(5) })
           .expect_no_data_loss()
           .expect_eventual_delivery()
           .run(&env).await?;
   }
   ```

3. **Expand TestEnv Helpers:**
   - Add `ingest_webhook_for_test()` helper
   - Add `create_batch_webhooks()` for load testing
   - Add `simulate_endpoint_failure()` for circuit breaker tests

#### Day 1-2: Exponential Backoff Implementation (TDD)

#### Day 3-4: Exponential Backoff Implementation

**Red Phase - Property tests:**

```rust
proptest! {
    fn backoff_increases_exponentially(attempt in 0u32..20)
    fn jitter_prevents_thundering_herd(seed in any::<u64>())
    fn respects_maximum_delay(attempt in 0u32..100)
}
```

**Green Phase - Core algorithm:**

- Base delay calculation (2^attempt \* base_delay)
- Jitter addition (±25% randomization)
- Maximum delay cap (1 hour)

**Refactor Phase - Configuration:**

- Per-endpoint backoff policies
- Configurable base delay and multiplier
- Smart retry scheduling

#### Day 5: Circuit Breaker Integration Tests

**Red Phase - Integration tests:**

```rust
#[tokio::test]
async fn worker_respects_open_circuit()
#[tokio::test]
async fn worker_probes_half_open_circuit()
async fn circuit_opens_after_failure_threshold()
```

### Phase 2: Production Operations (Days 6-7)

**Objective:** Add essential production features for deployment readiness

#### Day 6: Metrics & Monitoring

**Metrics to implement:**

- webhook_ingestion_total (counter)
- webhook_delivery_attempts_total (counter)
- webhook_delivery_duration_seconds (histogram)
- circuit_breaker_state (gauge)
- worker_pool_size (gauge)

**Health checks:**

- /health - Basic liveness
- /ready - Database connectivity
- /metrics - Prometheus endpoint

#### Day 7: Rate Limiting & Polish

**Per-tenant limits:**

- Ingestion rate limiting
- Delivery concurrency limits
- Burst allowances

**Implementation:**

- Token bucket algorithm
- Redis-backed distributed limiting
- Graceful degradation

**Load Testing Integration:**

- Use new TestEnv helpers for load test setup
- ScenarioBuilder for complex load scenarios
- Chaos testing for failure conditions under load

## Test-Driven Development Process

### TDD Workflow for Each Feature

1. **RED - Write Failing Test**
   - Define expected behavior
   - Write minimal test that fails
   - Verify test fails for right reason

2. **GREEN - Make Test Pass**
   - Write simplest code that passes
   - Don't optimize prematurely
   - Focus on correctness

3. **REFACTOR - Improve Code**
   - Remove duplication
   - Improve naming
   - Add documentation
   - Optimize if needed

### Test Pyramid Target

```
         /\
        /  \  5% - E2E Tests (Full system)
       /    \
      /------\  10% - Integration Tests (With database)
     /        \
    /----------\  15% - Scenario Tests (Multi-step)
   /            \
  /--------------\  25% - Property Tests (Invariants)
 /                \
/------------------\  45% - Unit Tests (Pure functions)
```

### Testing Standards

**Unit Tests:**

- No I/O operations
- No side effects
- Fast (<1ms each)
- Test single responsibility

**Integration Tests:**

- Use TestEnv infrastructure
- Real database operations
- Mock external services
- Test component boundaries

**Property Tests:**

- Generate edge cases
- Verify invariants
- 100+ cases in CI
- Regression prevention

**Scenario Tests:**

- Multi-step workflows
- Deterministic time (TestClock)
- State transitions
- Happy and sad paths

## Quality Gates

### Per-Commit Checks

- [ ] cargo fmt passes
- [ ] cargo clippy -- -D warnings
- [ ] All tests pass
- [ ] No new TODOs without issue links

### Per-Feature Checks

- [ ] Unit tests cover happy path
- [ ] Property tests cover edge cases
- [ ] Integration tests verify behavior
- [ ] Documentation updated
- [ ] Benchmarks show no regression

### Pre-Release Checks

- [ ] Load tests pass targets
- [ ] Security audit clean
- [ ] API backwards compatible
- [ ] Migration tested
- [ ] Monitoring in place

## Documentation Updates Needed

### README.md Improvements

1. Simplify landing page
2. Move details to docs/
3. Focus on value proposition
4. Clear getting started

### Missing Documentation

1. API reference (OpenAPI spec)
2. Deployment guide
3. Configuration reference
4. Troubleshooting guide
5. Architecture decision records

## Immediate Action Items

### Today (Priority Order):

1. **✅ COMPLETED: Remove TODOs and cleanup**
   - Implemented delivery_attempts recording
   - Removed migration TODO (using proper migrations)
   - Cleaned up README.md

2. **✅ COMPLETED: HTTP Client TDD**
   - Written comprehensive test suite (9 tests)
   - Confirmed HTTP client is fully functional
   - All individual tests pass

3. **URGENT: Fix Test Infrastructure** (Today)
   - Fix database connection pool exhaustion in parallel tests
   - Refactor golden sample test to use ScenarioBuilder
   - Add first chaos test as proof of concept

4. **Start Exponential Backoff TDD** (Next)
   - Write failing property tests for backoff calculation
   - Implement configurable backoff strategy
   - Integrate with worker delivery loop

### Tomorrow:

1. Complete test infrastructure improvements (ScenarioBuilder, chaos tests)
2. Complete exponential backoff implementation 3. Start circuit breaker integration tests with new test patterns

### This Week:

1. Complete Phase 1 (Testing Infrastructure & Core Reliability)
2. Complete Phase 2 (Production Operations)
3. Deploy to staging environment with chaos testing validation
4. Run first load tests using enhanced test harness

## Success Metrics

### Week 1 Goals

- [x] ✅ 100% test coverage on delivery path
- [x] ✅ Successful webhook delivery in tests
- [ ] ScenarioBuilder refactoring complete
- [ ] First chaos test implemented
- [ ] Exponential backoff working
- [ ] Circuit breaker integrated

### Week 2 Goals

- [ ] 10K webhooks/sec achieved
- [ ] p99 latency < 50ms
- [ ] Prometheus metrics exposed
- [ ] Load tests passing

### Month 1 Goals

- [ ] Production deployment
- [ ] First customer webhook delivered
- [ ] 99.9% delivery rate achieved
- [ ] Zero data loss incidents

## Risk Mitigation

### Technical Risks

- **Database bottleneck**: Use read replicas, connection pooling
- **Memory leaks**: Continuous profiling, bounded queues
- **Thundering herd**: Jitter in retry timing
- **Circuit breaker floods**: Gradual recovery in half-open

### Operational Risks

- **No monitoring**: Deploy metrics day 1
- **No rate limiting**: Basic limits before production
- **No runbooks**: Document common issues
- **No backup**: Database replication setup

## Conclusion

Kapsel has a solid foundation but needs focused execution to complete the delivery engine. The 2-week timeline is aggressive but achievable with disciplined TDD approach and clear priorities.

**Critical Path:**

1. ~~HTTP client~~ ✅ COMPLETE
2. Test infrastructure improvements (2 days)
3. Retry logic (2 days)
4. Circuit breaker (1 day)
5. Operations (2 days)

The test-first approach ensures we build exactly what's needed with confidence in correctness. Each feature will be proven through tests before implementation, reducing debugging time and increasing velocity.

**Major Discovery:** HTTP client is already complete and fully functional! This accelerates our timeline significantly.

Next step: Fix test infrastructure and implement ScenarioBuilder patterns immediately.
