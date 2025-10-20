# TODO

## Current State

**Complete:**

- Webhook ingestion with HMAC validation, idempotency, 10MB limits
- Delivery engine with exponential backoff, circuit breakers, worker pools
- Merkle attestation cryptography (rs-merkle, Ed25519 signatures)
- 292+ tests across unit/integration/property/chaos layers

**Critical Gaps:**

- **SECURITY:** Raw API keys stored in `tenants` table
- **ROBUSTNESS:** Event handler failures can crash delivery workers
- **SAFETY:** `unsafe` block in test harness for lifetime transmutation
- **CONFIG:** Environment variables defined but not parsed (using defaults)
- Circuit breaker recovery uses `force_circuit_state` in tests instead of real timeouts
- Attestation API missing (crypto complete, no HTTP routes)
- Multi-tenant HTTP isolation not validated end-to-end
- Rate limiting not implemented
- Prometheus metrics missing

---

## Priority 0: Security & Robustness [CRITICAL]

### API Key Storage Fix

**Why:** Raw `api_key` column in `tenants` table is a security vulnerability. If database is compromised, raw keys are exposed.

**Files:**

- `migrations/001_initial_schema.sql`
- `crates/kapsel-core/src/tenant/repository.rs`

- [ ] Remove `api_key` and `api_key_hash` columns from `tenants` table
- [ ] Use `api_keys` table as single source of truth
- [ ] Update repository methods to join with `api_keys` table
- [ ] Ensure keys are only shown once at generation, never stored raw

**Estimate:** 0.5 days

### Event Handler Isolation

**Why:** Subscriber panic crashes delivery worker, violating fault isolation. Secondary system failures (attestation) should never affect primary system (delivery).

**Files:**

- `crates/kapsel-core/src/events/handler.rs`
- `crates/kapsel-delivery/src/worker.rs`

- [ ] Spawn each `handle_event` call as detached task in `MulticastEventHandler`
- [ ] Add timeout for subscriber execution (30s default)
- [ ] Log subscriber failures without affecting delivery
- [ ] Test: Panicking subscriber doesn't crash worker
- [ ] Test: Slow subscriber doesn't block delivery

**Estimate:** 0.5 days

---

## Priority 1: Safety & Configuration

### Eliminate `unsafe` in Test Harness

**Why:** `unsafe` block transmutes lifetime to `'static`, breaking Rust safety guarantees. Symptom of ergonomic challenge in `TestEnv` design.

**Files:**

- `crates/kapsel-testing/src/database.rs`
- `crates/kapsel-testing/src/env.rs`

- [ ] Refactor to closure-based transaction management:
  ```rust
  pub async fn transaction<F, Fut, T>(&self, f: F) -> Result<T>
  where
      F: FnOnce(&mut sqlx::Transaction<'_, Postgres>) -> Fut,
      Fut: std::future::Future<Output = Result<T>>,
  ```
- [ ] Update all tests to use new pattern
- [ ] Remove `unsafe` block and `TODO` comment
- [ ] Verify no performance regression

**Estimate:** 1-2 days

### Full Configuration Implementation

**Why:** `.env.example` defines many variables but `Config::from_env()` ignores most. Production deployment would be misconfigured by default.

**Files:**

- `crates/kapsel-api/src/config.rs`
- `src/main.rs`

- [ ] Parse all variables from `.env.example`:
  - Worker pool size, channel capacity
  - Retry policies (max attempts, backoff parameters)
  - Circuit breaker thresholds and timeouts
  - Database connection pool settings
  - Attestation batch sizes
- [ ] Use `config-rs` or `figment` for type-safe parsing
- [ ] Add validation for required vs optional
- [ ] Test: All documented env vars are respected

**Estimate:** 1 day

---

## Priority 2: Circuit Breaker Recovery

**Why:** Recovery path advertised but never tested with real timeouts.

**File:** `crates/kapsel-delivery/tests/circuit_recovery_test.rs`

- [ ] `circuit_transitions_open_to_half_open_after_timeout()`
  - Trigger 5 failures → circuit opens
  - Wait for `open_timeout` (30s) using real time advance
  - Verify state transitions to `HalfOpen` automatically
  - Assert next request attempts delivery (probe behavior)

- [ ] `half_open_probe_request_limits()`
  - Circuit in `HalfOpen` state
  - Attempt multiple concurrent requests
  - Only `half_open_max_requests` (2) allowed through
  - Others get `CircuitOpen` error without HTTP attempt

- [ ] `half_open_success_closes_circuit()`
  - `HalfOpen` state with successful probe
  - Verify transitions to `Closed` after `success_threshold` (2) successes
  - Subsequent requests flow normally

- [ ] `half_open_failure_reopens_circuit()`
  - `HalfOpen` state with failed probe
  - Verify immediate transition back to `Open`
  - Reset `open_timeout` timer

**Estimate:** 2-3 days

---

## Priority 3: Attestation API

**Why:** Core value proposition. Backend crypto done, missing HTTP layer.

**Files:**

- `crates/kapsel-api/src/routes/attestation.rs`
- `crates/kapsel-api/tests/attestation_api_test.rs`

- [ ] `GET /attestation/sth` - Latest signed tree head
- [ ] `GET /attestation/proof/{leaf_hash}` - Inclusion proof
- [ ] `GET /attestation/download-proof/{delivery_attempt_id}` - Complete verification package
- [ ] Integration tests with end-to-end proof verification
- [ ] Update `server.rs` router (public endpoints, no auth)

**Estimate:** 3-4 days

---

## Priority 4: Multi-Tenant Security

**Why:** Database isolation exists but HTTP boundary not validated.

**File:** `tests/tenant_isolation_test.rs`

- [ ] `tenant_a_cannot_access_tenant_b_events_via_http()`
- [ ] `tenant_a_cannot_access_tenant_b_endpoints()`
- [ ] `database_queries_enforce_tenant_id_filtering()`
- [ ] Audit all SELECT queries for tenant_id filtering

**Estimate:** 1-2 days

---

## Priority 5: Testing Refinements

### TestEnv Worker Simulation

**Why:** `run_delivery_cycle` commits mid-test, breaking transaction isolation. Makes state harder to reason about.

**Files:**

- `crates/kapsel-testing/src/delivery.rs`
- Tests using `run_delivery_cycle`

- [ ] Worker should use own connection from pool, not test transaction
- [ ] Remove `is_custom_runtime()` checks
- [ ] Test assertions on separate connection
- [ ] Document transaction boundaries clearly

**Estimate:** 1 day

### Promote Unit Tests

**Why:** Current tests heavily favor integration style. Pure logic should have pure unit tests.

**Files:**

- All `src/` files with testable pure functions
- `docs/TESTING_STRATEGY.md`

- [ ] Extract pure logic from I/O-heavy functions
- [ ] Add inline `#[cfg(test)] mod tests` blocks
- [ ] No `TestEnv`, no `#[tokio::test]`, no I/O in unit tests
- [ ] Update TESTING_STRATEGY.md with clear boundaries
- [ ] Example: Request building logic in `client.rs`

**Estimate:** Ongoing

---

## Priority 6: Rate Limiting

**Why:** Zero protection against abuse.

**Files:**

- `crates/kapsel-api/src/middleware/rate_limit.rs`
- `tests/rate_limit_test.rs`

- [ ] Add `governor = "0.6"`, `tower-governor = "0.1"`
- [ ] Token bucket middleware per tenant_id
- [ ] HTTP 429 with `Retry-After` header
- [ ] Multi-tenant isolation testing

**Estimate:** 1-2 days

---

## Priority 7: Prometheus Metrics

**Why:** Zero observability in production.

**File:** `crates/kapsel-api/src/metrics.rs`

- [ ] Add `prometheus = "0.14"`
- [ ] Core metrics:
  - `webhook_ingestion_total{tenant,status}`
  - `delivery_attempts_total{endpoint,result}`
  - `delivery_duration_seconds` (histogram)
  - `circuit_breaker_state{endpoint}` (gauge)
- [ ] `/metrics` endpoint
- [ ] Test metrics incremented correctly

**Estimate:** 2 days

---

## Next Phase: Management APIs

- `GET /v1/events` - List events with pagination
- `GET /v1/events/{id}` - Event details
- `POST /v1/events/{id}/retry` - Manual retry
- `CRUD /v1/endpoints` - Endpoint management

---

## Later: Client Verification

- `kapsel-verification` crate (standalone)
- `kapsel-verify` CLI tool
- Multi-platform builds
- Property tests for all attack vectors

---

## Later: Performance

- Benchmarks to validate 10K events/sec
- Proof caching with LRU eviction
- Database query optimization
- Memory profiling under load

---

## Total Estimate

- **Week 1:** Security fixes + Configuration + Circuit recovery
- **Week 2:** Attestation API + Tenant security
- **Week 3:** Testing refinements + Rate limiting + Metrics
- **Week 4:** Management APIs

This depends on the number of Redbulls consumed.
