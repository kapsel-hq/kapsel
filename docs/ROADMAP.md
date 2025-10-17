# Kapsel Development Roadmap

**Vision**: Build the definitive webhook reliability service for regulated industries through cryptographically guaranteed proof-of-delivery at 50,000+ events/sec.

---

## Document Purpose

**ROADMAP.md**: Strategic technical plan covering 6 phases over 18 months with architectural milestones and feature delivery. For project planning and technical decision-making.

**IMPLEMENTATION_STATUS.md**: Tactical snapshot of current codebase state, what's working, what's incomplete, and immediate next steps. For engineering execution and sprint planning.

---

## Strategic Positioning

### Market Differentiation

Existing webhook platforms (Svix, Hookdeck) provide authentication via HMAC signatures but not non-repudiation. Symmetric HMAC requires shared secrets, limiting legal standing and third-party verifiability.

**Kapsel's approach**: Asymmetric cryptography (Ed25519) with Merkle tree inclusion proofs enabling external parties to verify delivery without trusting Kapsel.

### Target Use Cases

**Primary Applications**:
- Fintech: Payment processors, lending platforms requiring SOX/PCI DSS audit trails
- Healthcare: Patient notification systems needing HIPAA-compliant immutable logs
- Legal tech: Contract execution platforms requiring admissible evidence
- E-commerce: Order confirmation verification with tamper-proof records
- Government: FOIA-compliant immutable record systems

### Technical Comparison

| Capability | Competitors | Kapsel |
|------------|-------------|--------|
| Webhook delivery reliability | Yes | Yes |
| HMAC signature authentication | Yes | Yes |
| Multi-region deployment | Partial | Phase 4 |
| Circuit breaker protection | Yes | Yes |
| Third-party verifiable proofs | No | **Yes** |
| Ed25519 asymmetric signatures | No | **Yes** |
| Merkle tree attestation | No | **Yes** |
| Client verification tools | No | **Yes** |

---

## Technical Architecture

### Core Components

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

### Technology Stack

**Core Infrastructure**:
- Rust (memory safety, zero-cost abstractions)
- Axum (type-safe async HTTP)
- PostgreSQL (ACID compliance)
- Tokio (async runtime)

**Attestation Stack**:
- rs-merkle (RFC 6962 compliant Merkle trees)
- ed25519-dalek (digital signatures)
- OpenTimestamps (blockchain anchoring, Phase 6)

**Testing Infrastructure**:
- proptest (property-based testing)
- sqlx (compile-time SQL verification)
- testcontainers (isolated database tests)

### Performance Targets

| Metric | MVP (Phase 3) | Production (Phase 5) | Scale (Future) |
|--------|---------------|---------------------|----------------|
| Throughput | 10,000 events/sec | 25,000 events/sec | 50,000+ events/sec |
| Ingestion latency (p99) | <50ms | <20ms | <10ms |
| Attestation batch | <100ms | <100ms | <50ms |
| Proof generation (p99) | <100ms | <50ms | <20ms |
| Availability | 99.9% | 99.95% | 99.99% |

---

## Development Phases

### Phase 1: Foundation (Weeks 1-4) - Core Webhook Reliability

**Goal**: Prove webhook delivery works reliably at 1,000+ events/sec.

**Status**: 90% complete (see IMPLEMENTATION_STATUS.md)

**Completed Components**:
- PostgreSQL schema with multi-tenancy support
- HTTP ingestion API with HMAC validation
- Worker pool using SKIP LOCKED distribution
- Exponential backoff retry logic
- Circuit breaker state machine
- Comprehensive test infrastructure (135+ tests)

**Remaining Work**:
- HTTP delivery client implementation
- Circuit breaker integration with delivery decisions
- Production observability (Prometheus metrics, distributed tracing)

**Success Criteria**:
- Sustain 1,000 webhooks/sec for 1 hour
- Zero data loss under simulated failures
- Circuit breakers prevent cascade failures
- 80%+ test coverage maintained

---

### Phase 2: Cryptographic Foundation (Weeks 5-8) - Merkle Tree Attestation

**Goal**: Add cryptographically verifiable proof-of-delivery as core differentiator.

**Status**: Architecture defined, implementation ready to begin

#### Week 5-6: Schema & Crypto Services

**Database Migration** (002_merkle_attestation.sql):
- `attestation_keys`: Ed25519 key management with single active key constraint
- `merkle_leaves`: Append-only event log linking to delivery_attempts
- `signed_tree_heads`: Periodic commitments (tree_size, root_hash, Ed25519 signature)
- `proof_cache`: Pre-computed inclusion proofs for performance optimization

**SigningService Implementation**:
- Ed25519 key generation and secure storage
- Signing message format: `tree_size (8 bytes) || timestamp (8 bytes) || root_hash (32 bytes)`
- 64-byte Ed25519 signature output
- Key storage: Environment variables (development), HSM/KMS (production)

**MerkleService Implementation**:
- Batch leaf accumulation via non-blocking channel
- Tree construction using rs-merkle library
- STH generation trigger: every 10 seconds OR 100 pending events
- Inclusion proof generation with O(log N) complexity

**Success Criteria**:
- Ed25519 keys generated and persisted securely
- Merkle tree correctly builds with 1-10,000 leaves
- STH signatures verify successfully
- Property tests prove all leaves remain provable

#### Week 7-8: Integration & Batch Processing

**Attestation Capture Pipeline**:
- Hook into `delivery_attempts` table inserts
- Extract: delivery_attempt_id, event_id, endpoint_url, payload_hash, response_status, timestamp
- Queue events for batch processing via async channel
- Non-blocking capture (delivery never blocks on attestation)

**Batch Commit Worker**:
- Trigger conditions: 10 seconds elapsed OR 100 pending events OR graceful shutdown initiated
- Processing: compute leaf hashes → update in-memory tree → sign STH → persist atomically
- Target latency: <100ms per batch commit
- Error handling: failed batches go to dead letter queue for retry

**Proof Generation API**:
```
GET /api/v1/attestation/sth
    Returns: Latest signed tree head with signature

GET /api/v1/attestation/proof/:delivery_attempt_id
    Returns: Inclusion proof with tree metadata

GET /api/v1/attestation/download-proof/:delivery_attempt_id
    Returns: Complete proof package for offline verification
```

**Success Criteria**:
- Delivery attempts automatically captured in attestation pipeline
- STH commits on schedule with <100ms latency (p99)
- Inclusion proofs generate in <100ms (p99)
- End-to-end integration tests pass
- Attestation failures don't block webhook delivery

**Deliverables**:
- Working attestation pipeline (delivery → leaf → STH)
- Proof generation API endpoints
- Comprehensive test coverage for cryptographic operations
- Performance benchmarks documenting overhead

---

### Phase 3: Client Verification (Weeks 9-12) - Trustless Validation

**Goal**: Enable external parties to independently verify proofs without trusting Kapsel.

**Status**: Planned

#### Week 9-10: Verification Library

**Standalone Rust Crate** (`kapsel-verify`):
```rust
pub fn verify_proof(
    leaf_data: &LeafData,
    proof: &InclusionProof,
    sth: &SignedTreeHead,
    public_key: &PublicKey,
) -> Result<(), VerificationError>
```

**Three-Step Verification Algorithm**:
1. Verify leaf hash: SHA-256(0x00 || JSON(leaf_data)) per RFC 6962
2. Verify Merkle inclusion proof: reconstruct root from leaf + sibling hashes
3. Verify STH signature: Ed25519 signature validation with public key

**Property-Based Tests**:
- Valid proofs always verify successfully
- Invalid proofs always rejected
- Tampered data always detected
- Signature mismatches caught
- Verification deterministic (same inputs → same result)

#### Week 11-12: CLI Tool & Documentation

**CLI Verification Tool** (`kapsel-verify`):
```bash
# Download proof package
curl https://api.kapsel.io/attestation/download-proof/da_xyz > proof.json

# Verify independently (no trust in Kapsel required)
kapsel-verify --proof proof.json --public-key <hex>

# Expected output
Proof verified successfully!
  Webhook delivered to https://example.com/webhook
  Timestamp: 2024-01-15T10:30:45Z
  HTTP status: 200
  Cryptographically signed by Kapsel (Ed25519)
```

**Documentation Deliverables**:
- Technical explainer: "How Merkle Tree Attestation Works"
- Integration guide for compliance and audit teams
- Example proof packages with step-by-step verification
- Security model documentation (trust boundaries, threat model)

**Success Criteria**:
- CLI tool published to crates.io and GitHub releases
- Documentation includes working examples
- External developer successfully verifies proof independently
- Verification completes in <10ms
- Binaries available for major platforms (macOS, Linux, Windows)

**Deliverables**:
- Production-ready verification library
- Multi-platform CLI tool binaries
- Comprehensive documentation and examples
- Sample proof packages for testing

---

### Phase 4: Production Hardening (Weeks 13-20) - Enterprise Scale

**Goal**: Achieve 99.95% uptime with 25,000+ events/sec capacity.

**Status**: Future work

#### Week 13-16: Performance & Scalability

**Proof Caching Strategy**:
- Implement writes to proof_cache during proof generation
- Cache-first lookup strategy before computation
- Cache invalidation on tree updates
- TTL-based eviction for storage management

**Database Optimization**:
- PgBouncer for connection pooling (transaction mode)
- Read replicas for proof generation (separate read/write paths)
- Query optimization based on production access patterns
- Table partitioning for time-series data (delivery_attempts, merkle_leaves)

**Load Testing & Optimization**:
- Sustained 25,000 events/sec load for 24 hours
- Profile: p50/p99 latency, memory usage, CPU utilization, database connections
- Optimize: batch size tuning, commit frequency, worker pool sizing
- Identify and eliminate bottlenecks

**Multi-Region Architecture**:
- Primary region: us-east-1 (write operations)
- Read replicas: us-west-2, eu-west-1 (proof generation)
- Geographic routing for proof API requests
- Replication lag monitoring

**Success Criteria**:
- Sustain 25,000 events/sec continuously
- p99 ingestion latency <20ms
- p99 proof generation <50ms
- Memory usage <4GB at peak load
- Database replication lag <1 second

#### Week 17-20: Operational Excellence

**Monitoring & Alerting**:
- Prometheus: RED metrics (Rate, Errors, Duration) for all endpoints
- Grafana: dashboards for delivery rates, latency distributions, error rates, attestation pipeline
- Alert rules: downtime, performance degradation, attestation failures, replication lag

**Graceful Degradation**:
- Attestation failures never block webhook delivery
- Dead letter queue for failed batch commits
- Automatic reconciliation jobs to fill attestation gaps
- Circuit breaker for attestation service

**Disaster Recovery**:
- PostgreSQL streaming replication for high availability
- Point-in-time recovery (PITR) capability
- Merkle tree reconstruction procedure from stored leaves
- Documented runbooks for all failure scenarios
- Quarterly disaster recovery drills

**Security Hardening**:
- HSM integration for Ed25519 signing keys (AWS KMS, YubiHSM)
- Key rotation procedures (annual rotation + emergency rotation)
- Per-tenant rate limiting
- Automated SQL injection testing (sqlx compile-time verification)
- Regular security audits

**Success Criteria**:
- 99.95% uptime measured over 30 days
- Mean time to recovery (MTTR) <5 minutes
- All critical alerts have documented runbooks
- Successful disaster recovery drill completion
- Zero high-severity security findings

**Deliverables**:
- Production-grade infrastructure configuration
- Complete monitoring and alerting setup
- Security audit report
- Disaster recovery documentation and procedures

---

### Phase 5: Compliance Features (Months 6-12) - Regulated Industries

**Goal**: Build features required for regulated industry deployments (healthcare, finance, legal).

**Status**: Future work

#### Months 6-9: Audit & Compliance Infrastructure

**Enhanced Audit Logging**:
- Comprehensive audit trails for all administrative actions
- Immutable audit logs (append-only, tamper-evident)
- Audit log export API for external analysis
- Retention policies with automated enforcement

**Access Controls**:
- Role-based access control (RBAC) system
- Multi-factor authentication (MFA) for administrative access
- API key management with fine-grained permissions
- Session management with automatic timeout

**Data Protection**:
- Encryption at rest for sensitive data
- TLS 1.3+ for all network communication
- Key management integration (AWS KMS, HashiCorp Vault)
- Data retention and deletion policies

#### Months 9-12: Compliance Documentation

**Security Documentation**:
- System security architecture document
- Data flow diagrams showing data handling
- Threat model and risk assessment
- Security controls mapping

**Operational Documentation**:
- Standard operating procedures (SOPs)
- Incident response procedures
- Business continuity planning
- Disaster recovery procedures

**Deliverables**:
- Audit-ready infrastructure
- Comprehensive security documentation
- Compliance feature set for regulated industries

---

### Phase 6: Advanced Cryptographic Features (Months 13-18)

**Goal**: Advanced cryptographic capabilities for extended use cases.

**Status**: Future work

#### Blockchain Anchoring (Months 13-15)

**Implementation via OpenTimestamps**:
- Submit Merkle root hashes to OpenTimestamps calendar servers
- Bitcoin blockchain anchoring for legal-grade timestamping
- Cost: $10-50 per Bitcoin transaction (amortized across millions of events)
- Frequency: hourly anchoring (24 transactions/day)

**Proof Upgrade API**:
```
GET /api/v1/attestation/blockchain-proof/:delivery_attempt_id
Returns: OTS proof + Bitcoin block confirmation
```

**Success Criteria**:
- Merkle roots anchored to Bitcoin hourly
- Proof upgrade API returns Bitcoin block confirmations
- Independent verification against Bitcoin blockchain
- Documentation for legal admissibility

#### Consistency Proofs (Months 15-16)

**Append-Only Property Verification**:
- RFC 6962 Section 2.1.2 consistency proof algorithm implementation
- API: `GET /api/v1/attestation/consistency-proof?old_size=X&new_size=Y`
- Proof that old tree is prefix of new tree (no history modification)

**Success Criteria**:
- Consistency proofs verify append-only property
- Automated monitoring detects any tampering attempts
- Documentation explains security guarantees

#### Enterprise Multi-Tenancy (Months 16-18)

**Advanced Tenant Features**:
- Custom Ed25519 signing keys per tenant (BYOK - Bring Your Own Key)
- Private verification infrastructure deployment (on-premises or customer VPC)
- Compliance templates (SOX, PCI DSS, HIPAA preset configurations)
- Tenant-specific audit logs and compliance reports

**Success Criteria**:
- Enterprise tenant deployed with custom keys
- Private verification infrastructure documented
- Compliance templates validated

---

## Success Metrics by Phase

### Phase 1: Foundation (Weeks 1-4)
- Technical: Sustain 1,000 webhooks/sec
- Testing: Maintain 80%+ coverage
- Outcome: Production-ready webhook delivery service

### Phase 2: Cryptographic Foundation (Weeks 5-8)
- Technical: <100ms attestation batch commits
- Testing: Comprehensive property tests for crypto operations
- Outcome: Cryptographic differentiation proven

### Phase 3: Client Verification (Weeks 9-12)
- Technical: <10ms independent verification
- Adoption: External verification tools deployed
- Outcome: Trustless third-party validation operational

### Phase 4: Production Hardening (Weeks 13-20)
- Technical: 25,000 events/sec, 99.95% uptime
- Reliability: MTTR <5 minutes for all incidents
- Outcome: Enterprise-scale production deployment

### Phase 5: Compliance Features (Months 6-12)
- Features: Audit-ready infrastructure complete
- Documentation: Compliance documentation suite finished
- Outcome: Regulated industry deployment enabled

### Phase 6: Advanced Features (Months 13-18)
- Features: Blockchain anchoring, consistency proofs operational
- Adoption: Advanced cryptographic features in production
- Outcome: Extended cryptographic capabilities deployed

---

## Technical Risk Management

### Performance Risks

**Risk: Merkle tree operations don't scale to 50K events/sec**
- Mitigation: Batch processing optimization, aggressive proof caching, read replica scaling
- Monitoring: Track batch processing latency, tree size growth rate
- Fallback: Optimize for 25K events/sec initially, implement sharding if needed

**Risk: PostgreSQL becomes bottleneck at high throughput**
- Mitigation: Connection pooling, query optimization, table partitioning, read replicas
- Monitoring: Database CPU, I/O, connection count, replication lag
- Fallback: Consider distributed databases (CockroachDB, YugabyteDB) if needed

### Security Risks

**Risk: Ed25519 signing key compromise**
- Mitigation: HSM/KMS integration, strict access controls, comprehensive audit logging
- Monitoring: Key usage auditing, anomaly detection
- Response: Emergency key rotation procedure, customer notification protocol

**Risk: Merkle tree tampering attempt**
- Mitigation: Consistency proofs, automated verification, immutable storage
- Monitoring: Tree consistency checks, signature verification
- Response: Alert on verification failures, automated incident response

### Operational Risks

**Risk: Attestation service failures block webhook delivery**
- Mitigation: Async architecture, circuit breakers, dead letter queues
- Monitoring: Attestation pipeline health, batch commit success rate
- Response: Automatic degradation to delivery-only mode, reconciliation jobs

**Risk: Database corruption or data loss**
- Mitigation: ACID compliance, replication, regular backups, PITR
- Monitoring: Replication health, backup success, data consistency checks
- Response: Documented disaster recovery procedures, regular DR drills

---

## Decision Gates

### After Phase 2 (Week 8): Attestation Viability

**Evaluation Criteria**:
- Merkle tree commits consistently <100ms
- Proofs generate correctly for all tree sizes
- Property tests pass comprehensively
- Performance overhead acceptable

**Go Decision**: Proceed to Phase 3 (client verification)
**No-Go Decision**: Revisit architecture, consider alternative approaches

### After Phase 3 (Week 12): Verification Feasibility

**Evaluation Criteria**:
- Independent verification works correctly
- Verification completes in <10ms
- Documentation clear and complete
- External validation successful

**Go Decision**: Proceed to Phase 4 (production hardening)
**No-Go Decision**: Address verification issues, improve documentation

### After Phase 4 (Month 5): Production Readiness

**Evaluation Criteria**:
- 99.95% uptime demonstrated
- 25K events/sec capacity validated
- Incident response effective
- Monitoring comprehensive

**Go Decision**: Proceed to Phase 5 (compliance features)
**No-Go Decision**: Address operational issues before adding features

### After Phase 5 (Month 12): Compliance Readiness

**Evaluation Criteria**:
- Audit infrastructure complete
- Compliance documentation comprehensive
- Security controls implemented
- Ready for regulated deployments

**Go Decision**: Proceed to Phase 6 (advanced features)
**No-Go Decision**: Complete compliance requirements first

---

## Related Documentation

- [Implementation Status](./IMPLEMENTATION_STATUS.md) - Current codebase state and immediate tasks
- [System Overview](./OVERVIEW.md) - Technical architecture and design philosophy
- [Attestation Architecture](./ATTESTATION.md) - Merkle tree implementation details
- [Testing Strategy](./TESTING_STRATEGY.md) - Quality assurance methodology
- [Technical Specification](./SPECIFICATION.md) - Detailed requirements and API contracts

