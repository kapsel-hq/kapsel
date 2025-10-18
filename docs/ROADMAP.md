# Roadmap

## The Problem

Webhooks fail silently. Networks partition, servers crash, rate limits trigger. Your payment notification disappears into the void. No proof it was ever sent. No way to verify delivery actually happened.

HMAC signatures prove the sender but not delivery. Symmetric crypto means shared secrets. Anyone with the key can forge "delivery proof."

This is a hard problem.

## The Solution

Cryptographic proof of delivery using Merkle trees and Ed25519 signatures.

Every webhook delivery becomes a leaf in an append-only Merkle tree. Periodic batch commits create Signed Tree Heads (STH) with Ed25519 signatures. Clients get inclusion proofs they can verify independently without trusting us.

Attack the proof, attack the math. Good luck with that.

## Technical Architecture

```
Webhook Sources → HTTP Receiver → PostgreSQL
                      ↓                ↓
                 Worker Pool  ←  Event Queue
                      ↓
        HTTP Delivery + Circuit Breakers
                      ↓
              ┌───────┴────────┐
              ↓                ↓
         Destinations    Merkle Attestation
                              ↓
                       Signed Tree Heads (STH)
                              ↓
                    ┌─────────┴─────────┐
                    ↓                   ↓
              Ed25519 Signatures  Blockchain Anchoring
```

**Core Stack:**

- Rust 1.75+ (memory safety, zero-cost abstractions)
- PostgreSQL 14+ (ACID compliance, SKIP LOCKED work distribution)
- rs-merkle 1.5+ (RFC 6962 compliant)
- ed25519-dalek 2.1+ (fast signatures)
- Axum 0.8+ (async HTTP)
- Tokio 1.35+ (async runtime)

**Performance Targets:**

| Metric                  | MVP Target     | Production Target | Scale Target    |
| ----------------------- | -------------- | ----------------- | --------------- |
| Throughput              | 10K events/sec | 25K events/sec    | 50K+ events/sec |
| Ingestion latency (p99) | <50ms          | <20ms             | <10ms           |
| Attestation batch       | <100ms         | <100ms            | <50ms           |
| Proof generation (p99)  | <100ms         | <50ms             | <20ms           |
| Availability            | 99.9%          | 99.95%            | 99.99%          |

## Current State

**Complete:**

- Webhook ingestion with HMAC validation, idempotency, 10MB limits
- PostgreSQL schema with multi-tenancy support
- Worker pool using SKIP LOCKED distribution
- Exponential backoff retry logic with jitter (1s-512s)
- Circuit breaker state machine (Closed → Open → Half-Open)
- Merkle attestation with Ed25519 signatures (RFC 6962 compliant)
- 292+ tests across unit/integration/property/chaos/scenario layers
- Test infrastructure with property-based testing

**Critical Gaps:**

- Circuit breaker recovery path uses `force_circuit_state` in tests instead of real timeouts
- Attestation API missing (crypto complete, no HTTP routes)
- Multi-tenant HTTP isolation not validated end-to-end
- Rate limiting not implemented
- Prometheus metrics missing

**Database Schema Status:**

Complete tables: `tenants`, `endpoints`, `webhook_events`, `delivery_attempts`, `merkle_leaves`, `signed_tree_heads`, `proof_cache`, `attestation_keys`

## Phase 1: Core Reliability

Fix the gaps that block production deployment.

### Circuit Breaker Recovery

**Problem:** Current tests use `force_circuit_state()` instead of real timeout transitions.

**Implementation:**

- Time-based `Open → HalfOpen` transitions after `open_timeout` (30s)
- Probe request limiting in `HalfOpen` state (`half_open_max_requests` = 2)
- Success/failure recovery paths from `HalfOpen`
- Integration with delivery engine under concurrent load

**Files:** `crates/kapsel-delivery/tests/circuit_recovery_test.rs`

**Tests Required:**

- `circuit_transitions_open_to_half_open_after_timeout()`
- `half_open_probe_request_limits()`
- `half_open_success_closes_circuit()`
- `half_open_failure_reopens_circuit()`

### Attestation HTTP API

**Problem:** Crypto backend complete, missing HTTP routes.

**Implementation:**

Routes needed:

- `GET /attestation/sth` - Latest signed tree head
- `GET /attestation/proof/{leaf_hash}` - Inclusion proof with sibling hashes
- `GET /attestation/download-proof/{delivery_attempt_id}` - Complete verification package

**Response Formats:**

STH Response:

```json
{
  "tree_size": 12345,
  "root_hash": "3b7e72d..." (hex),
  "timestamp": 1234567890000,
  "signature": "a1b2c3d..." (hex),
  "public_key": "9f8e7d6..." (hex),
  "sth_id": "uuid-here"
}
```

Inclusion Proof Response:

```json
{
  "leaf_hash": "abc123...",
  "leaf_index": 5678,
  "tree_size": 12345,
  "proof_hashes": ["hash1", "hash2", ...],
  "root_hash": "def456..."
}
```

**Files:**

- `crates/kapsel-api/src/routes/attestation.rs`
- `crates/kapsel-api/tests/attestation_api_test.rs`
- Update `crates/kapsel-api/src/server.rs` router

### Multi-Tenant Security

**Problem:** Database isolation exists, HTTP boundary validation missing.

**Security Requirements:**

- One tenant cannot access another tenant's data via API calls
- HTTP 404 responses (not 403) to prevent information leakage
- All SELECT queries must include `WHERE tenant_id = $1` filter
- Compile-time verification using SQLx macros

**Files:** `tests/tenant_isolation_test.rs`

**Tests Required:**

- `tenant_a_cannot_access_tenant_b_events_via_http()`
- `tenant_a_cannot_access_tenant_b_endpoints()`
- `database_queries_enforce_tenant_id_filtering()`

### Rate Limiting

**Implementation:**

- Token bucket algorithm per tenant_id
- In-memory storage (Redis for multi-instance later)
- HTTP 429 responses with `Retry-After` header
- Configurable limits per tenant tier

**Dependencies:** `governor = "0.6"`, `tower-governor = "0.1"`

**Files:**

- `crates/kapsel-api/src/middleware/rate_limit.rs`
- `tests/rate_limit_test.rs`

### Prometheus Metrics

**Metrics Required:**

- Counters: `webhook_ingestion_total{tenant,status}`, `delivery_attempts_total{endpoint,result}`
- Histograms: `delivery_duration_seconds`, `merkle_batch_size`
- Gauges: `circuit_breaker_state{endpoint}`, `pending_events_total`

**Endpoint:** `GET /metrics` returning Prometheus format

**Dependencies:** `prometheus = "0.14"`

**Files:** `crates/kapsel-api/src/metrics.rs`

**Estimate:** 2-3 weeks

## Phase 2: Client Verification

Enable trustless proof verification without depending on Kapsel.

### Standalone Verification Library

**Crate:** `kapsel-verification` (zero Kapsel dependencies)

**Core Functionality:**

```rust
pub struct ProofVerifier {
    service_public_key: [u8; 32],
}

impl ProofVerifier {
    pub fn verify_inclusion_proof(
        &self,
        proof: &InclusionProof,
        sth: &SignedTreeHead
    ) -> Result<(), VerificationError> {
        // 1. Verify STH signature with Ed25519
        // 2. Verify inclusion proof against root hash
        // 3. Verify leaf hash matches delivery data
    }
}
```

**Three-Step Verification Algorithm:**

1. Verify leaf hash: `SHA-256(0x00 || JSON(leaf_data))` per RFC 6962
2. Verify Merkle inclusion proof: reconstruct root from leaf + sibling hashes
3. Verify STH signature: Ed25519 signature validation with public key

### CLI Verification Tool

**Binary:** `kapsel-verify`

**Usage:**

```bash
# Download proof package
curl https://api.kapsel.io/attestation/download-proof/da_xyz > proof.json

# Verify independently
kapsel-verify --proof proof.json --public-key <hex>

# Output
✓ Proof verified successfully!
  Webhook delivered to https://example.com/webhook
  Timestamp: 2024-01-15T10:30:45Z
  HTTP status: 200
  Cryptographically signed by Kapsel
```

**Multi-Platform Builds:** macOS (Intel + Apple Silicon), Linux (x86_64), Windows (x86_64)

### Property-Based Testing

**Invariants to Test:**

- Valid proofs always verify successfully
- Invalid proofs always rejected (tampered data, wrong keys, modified proofs)
- Verification is deterministic (same inputs → same result)
- All leaves in any tree size can be proven

```rust
proptest! {
    #[test]
    fn all_leaves_provable(leaf_count in 1usize..1000) {
        let tree = build_tree(leaf_count);
        for i in 0..leaf_count {
            let proof = tree.proof(&[i]);
            prop_assert!(proof.verify(tree.root(), i, tree.size()));
        }
    }
}
```

**Files:**

- `crates/kapsel-verification/src/lib.rs`
- `crates/kapsel-verification/tests/verification_test.rs`
- `crates/kapsel-verify/src/main.rs`

**Estimate:** 1-2 weeks

## Phase 3: Production Scale

Handle real load and operational concerns.

### Performance Benchmarking

**Benchmarks Required:**

- Ingestion throughput (target: 10K events/sec sustained)
- Delivery latency under load (target: p99 < 50ms)
- Database query profiling and optimization
- Memory usage under sustained load
- Proof generation latency vs tree size

**Files:** `benches/webhook_benchmarks.rs`

### Proof Caching Strategy

**Implementation:**

- LRU cache for frequently requested inclusion proofs
- Cache-first lookup before computation
- Cache invalidation on new tree commits
- TTL-based eviction for storage management
- Metrics tracking hit rates and cache performance

**Database Integration:**

```sql
CREATE TABLE proof_cache (
    id BIGSERIAL PRIMARY KEY,
    leaf_hash BYTEA NOT NULL,
    tree_size BIGINT NOT NULL,
    proof_hashes BYTEA[] NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE(leaf_hash, tree_size)
);
```

**Files:** `crates/kapsel-attestation/src/cache.rs`

### Management APIs

**Event Query API:**

- `GET /v1/events` - List events with pagination, filtering
- `GET /v1/events/{event_id}` - Event details with delivery history
- `POST /v1/events/{event_id}/retry` - Manual retry trigger

**Endpoint Management API:**

- `GET /v1/endpoints` - List endpoints
- `POST /v1/endpoints` - Create endpoint with validation
- `PUT /v1/endpoints/{id}` - Update endpoint configuration
- `DELETE /v1/endpoints/{id}` - Delete endpoint

**Query Parameters:**

- `status` (received, pending, delivered, failed)
- `endpoint_id` for filtering
- `since`, `until` for time ranges
- `page`, `limit` for pagination

**Tenant Isolation:** All queries must be tenant-scoped with proper filtering.

**Files:**

- `crates/kapsel-api/src/routes/management.rs`
- `crates/kapsel-api/src/routes/endpoints.rs`
- `crates/kapsel-api/tests/management_api_test.rs`

### Advanced Monitoring

**Structured Logging:**

- JSON format with correlation IDs
- Request/response tracing
- Error context preservation
- Configurable log levels (trace, debug, info, warn, error)

**Distributed Tracing:**

- OpenTelemetry integration
- Cross-service correlation
- Performance bottleneck identification

**Health Checks:**

- `/health/live` - Process alive (Kubernetes liveness)
- `/health/ready` - Database connected (Kubernetes readiness)
- `/health/startup` - Initialization complete

**Estimate:** 2-3 weeks

## Phase 4: Advanced Cryptography

Push the boundaries of cryptographic guarantees.

### Blockchain Anchoring

**Implementation via OpenTimestamps:**

- Submit Merkle root hashes to OpenTimestamps calendar servers
- Bitcoin blockchain anchoring for legal-grade timestamping
- Cost optimization: $10-50 per Bitcoin transaction amortized across millions of events
- Frequency: hourly anchoring (24 transactions/day)

**Proof Upgrade API:**

```
GET /api/v1/attestation/blockchain-proof/:delivery_attempt_id
Returns: OTS proof + Bitcoin block confirmation
```

**Legal Admissibility:** Documentation for court admissibility of blockchain-anchored proofs.

### Consistency Proofs

**Append-Only Property Verification:**

- RFC 6962 Section 2.1.2 consistency proof algorithm
- API: `GET /api/v1/attestation/consistency-proof?old_size=X&new_size=Y`
- Proof that old tree is prefix of new tree (no history modification)
- Automated monitoring detects tampering attempts

**Algorithm:**

```rust
pub fn verify_consistency(
    old_size: usize,
    new_size: usize,
    old_root: &[u8; 32],
    new_root: &[u8; 32],
    proof_hashes: &[[u8; 32]],
) -> bool {
    // Verify old tree is prefix of new tree
    // Per RFC 6962 Section 2.1.2
}
```

### Multi-Region Deployment

**Architecture:**

- Primary region: us-east-1 (write operations)
- Read replicas: us-west-2, eu-west-1 (proof generation)
- Geographic routing for proof API requests
- Replication lag monitoring and alerting

**Performance Targets:**

- Sub-100ms proof generation globally
- Database replication lag <1 second
- Automatic failover capabilities

### HSM Integration

**Key Management:**

- Ed25519 keys in HSM/KMS (AWS KMS, YubiHSM)
- Key rotation procedures (annual + emergency)
- Backward compatibility for proof verification

**Development vs Production:**

```rust
// Development
let signing_key = SigningKey::from_bytes(&KEY_BYTES);

// Production
let signing_key = kms_client
    .get_signing_key("kapsel-attestation-key")
    .await?;
```

**Estimate:** 1-2 months

## Phase 5: Next-Generation Features

Research-grade cryptography and advanced use cases.

### Zero-Knowledge Proofs

**Privacy-Preserving Verification:**

- Prove delivery happened without revealing payload content
- zk-SNARK proof aggregation for efficiency
- Selective disclosure for sensitive webhooks

**Use Cases:**

- Healthcare: Prove patient notification sent without revealing medical data
- Finance: Prove payment notification without exposing amounts
- Legal: Prove contract notifications without revealing terms

### BYOK Enterprise

**Bring Your Own Key Deployments:**

- Custom Ed25519 keys per enterprise tenant
- Private attestation infrastructure deployment
- Isolated verification key distribution
- Tenant-specific compliance templates

**Architecture:**

- On-premises or customer VPC deployment
- Air-gapped key generation and management
- Custom attestation service instances

### Threshold Signatures

**Distributed Key Generation:**

- Split Ed25519 signing across multiple HSMs
- Byzantine fault tolerance for signing operations
- No single point of cryptographic failure
- Shamir's Secret Sharing for key recovery

**Security Benefits:**

- Requires compromise of multiple HSMs
- Gradual key reveal detection
- Disaster recovery without key loss

**Estimate:** 6+ months

## Database Optimization Strategy

### Connection Pooling

**PgBouncer Configuration:**

- Transaction mode for optimal connection reuse
- Separate pools for read/write operations
- Connection limits per tenant

### Query Optimization

**Indexing Strategy:**

```sql
-- Critical indexes for performance
CREATE INDEX idx_webhook_events_tenant_status ON webhook_events(tenant_id, status);
CREATE INDEX idx_delivery_attempts_event_created ON delivery_attempts(event_id, created_at);
CREATE INDEX idx_merkle_leaves_delivery_attempt ON merkle_leaves(delivery_attempt_id);
CREATE INDEX idx_merkle_leaves_leaf_hash ON merkle_leaves(leaf_hash);
```

### Table Partitioning

**Time-Series Partitioning:**

- `delivery_attempts` partitioned by month
- `merkle_leaves` partitioned by tree commitment date
- Automated partition maintenance

## Security Threat Model

### Attack Vectors

**Ed25519 Private Key Compromise:**

- Mitigation: HSM integration, strict access controls, audit logging
- Detection: Key usage monitoring, anomaly detection
- Response: Emergency key rotation, customer notification

**Merkle Tree Tampering:**

- Mitigation: Consistency proofs, automated verification, immutable storage
- Detection: Tree consistency checks, signature verification failures
- Response: Automated incident response, forensic analysis

**Multi-Tenant Data Leakage:**

- Mitigation: Compile-time query verification, HTTP boundary testing
- Detection: Access pattern monitoring, tenant isolation validation
- Response: Immediate tenant notification, security audit

### Operational Security

**Secrets Management:**

- All secrets in environment variables or KMS
- No secrets in logs, error messages, or responses
- Automatic secret rotation procedures

**Network Security:**

- TLS 1.3+ for all external communication
- mTLS for internal service communication
- Network segmentation and firewall rules

## Performance Risk Mitigation

### Scaling Bottlenecks

**Problem:** Merkle tree operations don't scale to 50K events/sec
**Mitigation:** Batch processing optimization, aggressive proof caching, read replica scaling
**Monitoring:** Batch processing latency, tree size growth rate
**Fallback:** Database sharding, distributed tree construction

**Problem:** PostgreSQL becomes bottleneck at high throughput
**Mitigation:** Connection pooling, query optimization, table partitioning, read replicas
**Monitoring:** Database CPU, I/O, connection count, replication lag
**Fallback:** Distributed databases (CockroachDB, YugabyteDB)

### Memory Management

**Tree Size Growth:** In-memory Merkle tree grows with number of leaves
**Mitigation:** Tree pruning strategies, disk-backed tree storage, tree sharding
**Monitoring:** Memory usage, tree reconstruction time

## Decision Gates

### After Phase 1: Production Readiness

**Evaluation Criteria:**

- Circuit breaker recovery tested under all failure modes
- Attestation API generating proofs at target latency
- Multi-tenant security validated end-to-end
- Rate limiting preventing abuse scenarios

**Go/No-Go Decision:** All critical gaps closed, security audit passed

### After Phase 2: Verification Feasibility

**Evaluation Criteria:**

- Independent verification works correctly across platforms
- CLI tool adopted by external users
- Property tests cover all attack vectors
- Documentation enables successful integration

**Go/No-Go Decision:** Trustless verification proven, adoption metrics positive

### After Phase 3: Scale Validation

**Evaluation Criteria:**

- 10K events/sec sustained without degradation
- Management APIs powering production dashboards
- Monitoring detecting and alerting on issues
- Operational runbooks complete and tested

**Go/No-Go Decision:** Production scale achieved, operational excellence demonstrated

## Success Metrics

**Technical Metrics:**

- Sustain 10K+ events/sec continuously
- p99 ingestion latency <50ms
- p99 proof generation <100ms
- 99.95% uptime over 30-day periods
- Zero critical security vulnerabilities

**Adoption Metrics:**

- CLI verification tool usage
- API integration patterns
- Customer retention and growth
- Community contributions and feedback

**Business Metrics:**

- Cryptographic differentiation proven valuable
- Customer migration from competitors
- Enterprise contract wins
- Revenue growth trajectory

## Why This Matters

**Trustless Verification:** Anyone can verify proofs without trusting Kapsel. Math doesn't lie, systems do.

**Legal Admissibility:** Cryptographic proofs hold up in court. No "he said, she said" disputes about delivery.

**Regulatory Compliance:** Immutable audit trails for SOX, HIPAA, PCI DSS. Seven-year retention with tamper evidence.

**Technical Moats:** Asymmetric cryptography and Merkle trees create defensible differentiation. Hard to replicate correctly.

## The Hard Parts

**Cryptographic Correctness:** One bug in signature verification or Merkle tree construction breaks everything. Extensive property testing and formal verification needed.

**Performance at Scale:** Merkle trees grow logarithmically but proof generation load grows linearly. Caching strategies and read replicas essential.

**Key Management:** Ed25519 private keys are single points of failure. HSM integration and key rotation procedures critical for production.

**Operational Complexity:** Multi-region deployment with cryptographic consistency is hard. Clock synchronization, replication lag, disaster recovery all matter.

**Customer Education:** Explaining cryptographic guarantees to non-technical buyers is challenging. Clear documentation and proof-of-concept demos essential.

## The Vision

Build the most reliable webhook service ever created. Not just promises of reliability - cryptographic guarantees. Not just logs you have to trust - mathematical proofs anyone can verify.

When someone asks "Did this webhook get delivered?", the answer isn't "Trust us, check our logs." The answer is "Here's the mathematical proof. Verify it yourself. Don't trust us, trust the math."

That's worth building.
