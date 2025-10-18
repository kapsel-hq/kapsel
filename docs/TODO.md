# TODO

## Current State

**Complete:**

- Webhook ingestion with HMAC validation, idempotency, 10MB limits
- Delivery engine with exponential backoff, circuit breakers, worker pools
- Merkle attestation cryptography (rs-merkle, Ed25519 signatures)
- 292+ tests across unit/integration/property/chaos layers

**Critical Gaps:**

- Circuit breaker recovery uses `force_circuit_state` in tests instead of real timeouts
- Attestation API missing (crypto complete, no HTTP routes)
- Multi-tenant HTTP isolation not validated end-to-end
- Rate limiting not implemented
- Prometheus metrics missing

---

## Priority 1: Circuit Breaker Recovery

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

## Priority 2: Attestation API

**Why:** Core value proposition. Backend crypto done, missing HTTP layer.

**Files:**

- `crates/kapsel-api/src/routes/attestation.rs`
- `crates/kapsel-api/tests/attestation_api_test.rs`

- [ ] `GET /attestation/sth`

  ```rust
  pub async fn latest_sth(
      State(merkle_service): State<Arc<RwLock<MerkleService>>>
  ) -> Result<Json<SignedTreeHead>, ApiError> {
      let service = merkle_service.read().await;
      let sth = service.latest_sth().await?;
      Ok(Json(sth.ok_or(ApiError::NotFound)?))
  }
  ```

- [ ] `GET /attestation/proof/{leaf_hash}`
  - Generate inclusion proof for delivery attempt
  - Support `?tree_size=N` for historical proofs
  - Return proof with sibling hashes and metadata

- [ ] `GET /attestation/download-proof/{delivery_attempt_id}`
  - Complete verification package: leaf data + proof + STH
  - Include verification instructions for CLI

- [ ] Integration tests
  - `sth_endpoint_returns_valid_signed_tree_head()`
  - `proof_endpoint_generates_verifiable_inclusion_proof()`
  - End-to-end proof verification using rs-merkle

- [ ] Update `server.rs` router (public endpoints, no auth)

**Estimate:** 3-4 days

---

## Priority 3: Multi-Tenant Security

**Why:** Database isolation exists but HTTP boundary not validated.

**File:** `tests/tenant_isolation_test.rs`

- [ ] `tenant_a_cannot_access_tenant_b_events_via_http()`
  - Start real Axum server with `TestEnv::with_server()`
  - Create tenants with separate API keys
  - Ingest webhook for Tenant B, get `event_id`
  - HTTP GET `/v1/events/{event_id}` with Tenant A's key
  - Assert: HTTP 404 (not 403), no data leakage

- [ ] `tenant_a_cannot_access_tenant_b_endpoints()`
  - Create endpoint for Tenant B
  - Access with Tenant A's credentials
  - Verify 404 with no metadata exposure

- [ ] `database_queries_enforce_tenant_id_filtering()`
  - Audit all SELECT queries include `WHERE tenant_id = $1`
  - Use SQLx compile-time verification
  - Document whitelisted system queries

**Estimate:** 1-2 days

---

## Priority 4: Rate Limiting

**Why:** Zero protection against abuse.

**Files:**

- `crates/kapsel-api/src/middleware/rate_limit.rs`
- `tests/rate_limit_test.rs`

- [ ] Add `governor = "0.6"`, `tower-governor = "0.1"`
- [ ] Token bucket middleware per tenant_id
  - In-memory storage (Redis later)
  - HTTP 429 with `Retry-After` header
- [ ] Tests: 101 req/sec (limit 100) → 101st gets 429
- [ ] Multi-tenant isolation testing

---

## Priority 5: Prometheus Metrics

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

- **Week 1:** Circuit recovery + Attestation API
- **Week 2:** Tenant security + Rate limiting + Metrics
- **Week 3:** Management APIs
- **Week 4:** Client verification tools

This depends on the number of Redbulls consumed.
