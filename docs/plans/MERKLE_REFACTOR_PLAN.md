# Merkle Tree Attestation Refactoring Plan

## Overview

This plan details the step-by-step implementation of Merkle tree-based attestation for Kapsel, following TIGERSTYLE naming conventions and TDD principles. Each phase represents a logical commit boundary.

## Phase 1: Database Foundation (Day 1)

### Task 1.1: Create Attestation Migration

**Commit**: `feat(db): add merkle attestation schema`

```sql
-- migrations/002_merkle_attestation.sql
-- Following existing schema patterns from 001_initial_schema.sql
-- Using TIGERSTYLE: no get/set prefixes, clear constraints
```

Schema elements:

- `merkle_leaves` - Append-only delivery event log
- `signed_tree_heads` - Periodic cryptographic commitments
- `proof_cache` - Performance optimization
- `attestation_keys` - Ed25519 key management

**Test First**:

```rust
// tests/attestation_schema_test.rs
#[tokio::test]
async fn merkle_tables_exist_after_migration() {
    let env = TestEnv::new().await.unwrap();

    // Verify tables exist
    let tables = env.list_tables().await.unwrap();
    assert!(tables.contains("merkle_leaves"));
    assert!(tables.contains("signed_tree_heads"));
}
```

### Task 1.2: Update Test Harness Schema

**Commit**: `fix(testing): add attestation tables to test schema`

Update `crates/kapsel-testing/src/database.rs` to include new tables. Must match migration exactly to prevent schema drift.

**Validation**: Run existing test suite - all 135+ tests must pass.

---

## Phase 2: Core Domain Models (Day 1-2)

### Task 2.1: Create Attestation Types

**Commit**: `feat(core): add merkle attestation domain models`

Location: `crates/kapsel-core/src/attestation/`

```rust
// Following existing patterns from webhook.rs
pub struct MerkleLeaf {
    pub leaf_index: i64,
    pub leaf_hash: [u8; 32],
    pub delivery_attempt_id: DeliveryAttemptId,
    // ...
}

pub struct SignedTreeHead {
    pub tree_size: i64,
    pub root_hash: [u8; 32],
    // ...
}
```

**Naming Conventions**:

- Types: `MerkleLeaf`, not `MerkleLeafData`
- Functions: `compute_leaf_hash()`, not `get_leaf_hash()`
- Predicates: `is_valid()`, not `validate()`

**Test First**:

```rust
#[test]
fn leaf_hash_computation_is_deterministic() {
    let leaf = create_test_leaf();
    let hash1 = leaf.compute_leaf_hash();
    let hash2 = leaf.compute_leaf_hash();
    assert_eq!(hash1, hash2);
}
```

### Task 2.2: Implement Cryptographic Operations

**Commit**: `feat(crypto): add merkle and signing operations`

Location: `crates/kapsel-core/src/crypto/`

```rust
pub mod merkle {
    // TIGERSTYLE: verb_object naming
    pub fn compute_leaf_hash(data: &LeafData) -> [u8; 32]
    pub fn hash_internal_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32]
    pub fn verify_inclusion_proof(...) -> bool
}

pub mod signing {
    pub fn sign_tree_head(...) -> Result<Signature>
    pub fn verify_tree_signature(...) -> Result<()>
}
```

**Property Tests**:

```rust
proptest! {
    #[test]
    fn merkle_tree_is_deterministic(leaves in vec(any::<[u8; 32]>(), 1..100)) {
        let tree1 = build_tree(&leaves);
        let tree2 = build_tree(&leaves);
        prop_assert_eq!(tree1.root(), tree2.root());
    }
}
```

---

## Phase 3: Service Layer (Day 2-3)

### Task 3.1: Create MerkleService

**Commit**: `feat(attestation): implement merkle service for tree management`

Location: `crates/kapsel-attestation/src/merkle_service.rs`

Following existing service patterns from `delivery_service.rs`:

```rust
pub struct MerkleService {
    db: PgPool,
    signing_service: Arc<SigningService>,
    tree: Arc<RwLock<MerkleTree>>,
    pending_leaves: Arc<RwLock<Vec<LeafData>>>,
}

impl MerkleService {
    // TIGERSTYLE: verb_object, no get_ prefix
    pub async fn add_delivery_event(...) -> Result<()>
    pub async fn commit_pending_leaves() -> Result<Option<SignedTreeHead>>
    pub async fn generate_inclusion_proof(...) -> Result<MerkleProof>
    pub async fn latest_sth() -> Result<Option<SignedTreeHead>>
    pub async fn rebuild_tree_from_sth() -> Result<()>
}
```

**Integration Test**:

```rust
#[tokio::test]
async fn merkle_service_batches_events() {
    let env = TestEnv::new().await.unwrap();
    let service = create_merkle_service(&env.db).await;

    // Add events
    for i in 0..10 {
        service.add_delivery_event(create_test_event(i)).await.unwrap();
    }

    // Commit batch
    let sth = service.commit_pending_leaves().await.unwrap();
    assert!(sth.is_some());
    assert_eq!(sth.unwrap().tree_size, 10);
}
```

### Task 3.2: Create SigningService

**Commit**: `feat(attestation): add ed25519 signing service`

Location: `crates/kapsel-attestation/src/signing_service.rs`

```rust
pub struct SigningService {
    signing_key: Arc<RwLock<SigningKey>>,
    public_key: VerifyingKey,
}

impl SigningService {
    pub async fn sign_tree_head(...) -> Result<Vec<u8>>
    pub fn public_key_bytes(&self) -> Vec<u8>
    pub fn verify_signature(&self, message: &[u8], signature: &[u8]) -> Result<()>
}
```

**Security Test**:

```rust
#[test]
fn signature_verification_roundtrip() {
    let service = SigningService::new_test();
    let message = b"test message";
    let signature = service.sign(message).unwrap();
    assert!(service.verify(message, &signature).is_ok());
}
```

---

## Phase 4: Delivery Integration (Day 3)

### Task 4.1: Capture Delivery Events

**Commit**: `feat(delivery): integrate attestation capture with delivery pipeline`

Modify `crates/kapsel-delivery/src/worker.rs`:

```rust
impl DeliveryWorker {
    async fn process_delivery(&self, event: WebhookEvent) -> Result<()> {
        // Existing delivery logic...

        // Add attestation capture (if enabled)
        if self.attestation_enabled {
            self.capture_delivery_attempt(&event, &response).await?;
        }
    }

    async fn capture_delivery_attempt(...) -> Result<()> {
        self.merkle_service
            .add_delivery_event(...)
            .await
    }
}
```

**End-to-End Test**:

```rust
#[tokio::test]
async fn delivery_creates_attestation_leaf() {
    let env = TestEnv::new().await.unwrap();

    // Create and deliver webhook
    let event = env.create_webhook_event().await;
    env.deliver_webhook(&event).await.unwrap();

    // Verify leaf created
    let leaf = env.db.maybe_fetch_leaf_for_event(&event.id).await.unwrap();
    assert!(leaf.is_some());
}
```

### Task 4.2: Background Commitment Worker

**Commit**: `feat(attestation): add periodic batch commitment worker`

```rust
pub async fn run_commitment_worker(
    service: Arc<MerkleService>,
    interval_secs: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;

        match service.commit_pending_leaves().await {
            Ok(Some(sth)) => {
                info!("Committed STH: tree_size={}", sth.tree_size);
            }
            Ok(None) => {} // No pending leaves
            Err(e) => {
                error!("Failed to commit leaves: {}", e);
            }
        }
    }
}
```

---

## Phase 5: API Endpoints (Day 4)

### Task 5.1: Attestation Routes

**Commit**: `feat(api): add attestation endpoints`

Location: `crates/kapsel-api/src/routes/attestation.rs`

```rust
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/attestation/sth", get(fetch_latest_sth))
        .route("/attestation/proof/:leaf_hash", get(generate_inclusion_proof))
        .route("/attestation/download-proof/:delivery_id", get(download_proof_package))
}

async fn fetch_latest_sth(
    State(state): State<AppState>,
) -> Result<Json<SignedTreeHead>, ApiError> {
    state.merkle_service
        .latest_sth()
        .await?
        .ok_or(ApiError::NotFound)
}
```

**API Test**:

```rust
#[tokio::test]
async fn sth_endpoint_returns_latest() {
    let env = TestEnv::new().await.unwrap();

    // Create some deliveries
    env.create_and_commit_leaves(10).await;

    // Fetch STH
    let response = env.client
        .get("/attestation/sth")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let sth: SignedTreeHead = response.json().await.unwrap();
    assert_eq!(sth.tree_size, 10);
}
```

---

## Phase 6: Client Verification (Day 5)

### Task 6.1: Verification Library

**Commit**: `feat(verification): add client-side proof verification library`

New crate: `crates/kapsel-verification/`

```rust
pub struct ProofVerifier {
    service_public_key: VerifyingKey,
}

impl ProofVerifier {
    pub fn verify(&self, proof: &ProofPackage) -> Result<()> {
        self.verify_leaf_hash(proof)?;
        self.verify_inclusion_proof(proof)?;
        self.verify_sth_signature(proof)?;
        Ok(())
    }
}
```

### Task 6.2: CLI Tool

**Commit**: `feat(cli): add kapsel-verify command for proof validation`

Location: `crates/kapsel-cli/src/verify.rs`

```rust
use clap::Parser;

#[derive(Parser)]
pub struct VerifyCommand {
    #[arg(long)]
    proof: PathBuf,

    #[arg(long)]
    public_key: String,
}

pub async fn execute(cmd: VerifyCommand) -> Result<()> {
    let proof = load_proof_package(&cmd.proof)?;
    let verifier = ProofVerifier::new(&cmd.public_key)?;

    match verifier.verify_proof(&proof) {
        Ok(()) => println!("✓ Proof verified successfully"),
        Err(e) => bail!("✗ Verification failed: {}", e),
    }
}
```

---

## Phase 7: Performance & Optimization (Week 2)

### Task 7.1: Proof Caching

**Commit**: `perf(attestation): implement proof caching for common queries`

### Task 7.2: Batch Size Tuning

**Commit**: `perf(attestation): optimize batch processing thresholds`

### Task 7.3: Tree Reconstruction

**Commit**: `feat(attestation): implement startup tree reconstruction from STH`

---

## Testing Strategy

### Test Categories by Phase

**Phase 1-2**: Unit tests for crypto operations

- Leaf hash computation
- Merkle tree construction
- Ed25519 signatures

**Phase 3-4**: Integration tests with database

- Event capture pipeline
- Batch processing timing
- STH persistence

**Phase 5**: API tests

- Endpoint availability
- Proof generation correctness
- Error handling

**Phase 6**: End-to-end verification

- Complete proof package
- Client-side validation
- Cross-platform compatibility

### Property Testing Focus

```rust
// Critical invariants to test
proptest! {
    // 1. All leaves are provable
    // 2. Tree is deterministic
    // 3. Signatures are valid
    // 4. Append-only property holds
    // 5. Batch size doesn't affect root
}
```

---

## Configuration

Add to `kapsel.toml`:

```toml
[attestation]
enabled = true
batch_timeout_ms = 10000
batch_size_threshold = 100
max_batch_size = 1000

[attestation.signing]
key_type = "ed25519"
# Development only - use KMS in production
private_key_hex = "..."
```

---

## Success Criteria

Each phase must meet:

1. **Tests Pass**: All existing tests remain green
2. **New Tests**: TDD - red, green, refactor cycle
3. **Performance**: No regression in ingestion throughput
4. **Documentation**: Updated for new features
5. **Style**: Follows TIGERSTYLE conventions

## Commit Schedule

- **Day 1**: Database + Domain Models (Tasks 1.1-2.2)
- **Day 2**: Core Services (Tasks 3.1-3.2)
- **Day 3**: Delivery Integration (Tasks 4.1-4.2)
- **Day 4**: API Endpoints (Task 5.1)
- **Day 5**: Client Verification (Tasks 6.1-6.2)
- **Week 2**: Optimization + Production Readiness

## Risk Mitigation

- **Schema Drift**: Validate test harness matches migration after each change
- **Performance**: Benchmark after each phase, abort if > 10% regression
- **Complexity**: Keep services focused, single responsibility
- **Testing**: Maintain > 80% coverage, 100% for crypto operations

## Next Steps

1. Start with Phase 1: Create migration file
2. Follow TDD: Write failing test first
3. Implement minimum to pass
4. Refactor for clarity
5. Commit with conventional message
6. Move to next task

Ready to begin? Start with:

```bash
cargo make tdd
# Create tests/attestation_schema_test.rs
# Watch it fail (RED)
# Create migration
# Watch it pass (GREEN)
```
