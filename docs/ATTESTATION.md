# Merkle Tree Attestation Architecture

Kapsel provides cryptographically verifiable proof of webhook delivery through Merkle tree-based attestation with Ed25519 signatures. This document defines the implementation architecture, data structures, and verification protocol.

## Core Concept

Every delivery attempt becomes an immutable leaf in an append-only Merkle tree. Periodic commitments produce Signed Tree Heads (STH) that prove:

- **What** was delivered (payload hash)
- **When** it was delivered (timestamp)
- **Where** it was delivered (endpoint URL)
- **Result** of delivery (HTTP status, response)
- **Authenticity** (Ed25519 signature by Kapsel)

## Architecture Components

### Data Flow

```
Delivery Attempt → Merkle Leaf → Batch Queue → Tree Commitment → Signed Tree Head
                        ↓                            ↓                 ↓
                   PostgreSQL                    Merkle Root     Ed25519 Signature
                                                     ↓                 ↓
                                               Inclusion Proof  Client Verification
```

### Component Responsibilities

**MerkleService** - Tree construction and proof generation

- Batch leaf processing (every 10s or 100 events)
- Merkle root computation using rs-merkle
- Inclusion proof generation with caching
- Consistency proof between STHs

**SigningService** - Ed25519 cryptographic operations

- Key generation and secure storage
- Tree head signing (root + size + timestamp)
- Public key management for verification
- Key rotation support (future)

**AttestationCapture** - Delivery event interception

- Extract delivery attempt data
- Compute payload and response hashes
- Queue events for batch processing
- Maintain event ordering

## Database Schema

### merkle_leaves

Append-only log of all delivery attempts:

```sql
CREATE TABLE merkle_leaves (
    leaf_index BIGSERIAL PRIMARY KEY,
    leaf_hash BYTEA NOT NULL UNIQUE,              -- SHA-256 of leaf_data
    delivery_attempt_id TEXT NOT NULL REFERENCES delivery_attempts(id),
    message_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    endpoint_url TEXT NOT NULL,
    payload_hash BYTEA NOT NULL,                  -- SHA-256 of webhook body
    timestamp_sent BIGINT NOT NULL,               -- Unix milliseconds
    http_status_code INTEGER,
    response_hash BYTEA,                          -- SHA-256 of response body
    leaf_data JSONB NOT NULL,                     -- Complete structured data
    tree_size_at_inclusion BIGINT,                -- STH that includes this leaf
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_merkle_leaves_delivery_attempt ON merkle_leaves(delivery_attempt_id);
CREATE INDEX idx_merkle_leaves_leaf_hash ON merkle_leaves(leaf_hash);
CREATE INDEX idx_merkle_leaves_tree_size ON merkle_leaves(tree_size_at_inclusion);
```

### signed_tree_heads

Periodic cryptographic commitments:

```sql
CREATE TABLE signed_tree_heads (
    tree_size BIGINT PRIMARY KEY,                 -- Number of leaves
    root_hash BYTEA NOT NULL,                     -- Merkle tree root
    timestamp BIGINT NOT NULL,                    -- Unix milliseconds
    signature BYTEA NOT NULL,                     -- Ed25519 signature (64 bytes)
    public_key BYTEA NOT NULL,                    -- Ed25519 public key (32 bytes)
    sth_id UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    CHECK (tree_size > 0),
    CHECK (LENGTH(root_hash) = 32),
    CHECK (LENGTH(signature) = 64),
    CHECK (LENGTH(public_key) = 32)
);

CREATE INDEX idx_sth_timestamp ON signed_tree_heads(timestamp);
```

### proof_cache

Performance optimization for common proof requests:

```sql
CREATE TABLE proof_cache (
    id BIGSERIAL PRIMARY KEY,
    leaf_hash BYTEA NOT NULL,
    tree_size BIGINT NOT NULL,
    proof_hashes BYTEA[] NOT NULL,                -- Array of sibling hashes
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE(leaf_hash, tree_size)
);

CREATE INDEX idx_proof_cache_leaf ON proof_cache(leaf_hash);
```

## Leaf Data Structure

Each delivery attempt is structured as:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafData {
    pub delivery_attempt_id: String,
    pub message_id: String,
    pub endpoint_url: String,
    pub payload_hash: String,      // hex-encoded SHA-256
    pub timestamp_sent: i64,        // Unix milliseconds
    pub http_status_code: Option<i32>,
    pub response_hash: Option<String>,  // hex-encoded SHA-256
}
```

Leaf hash computation (RFC 6962 compliant):

```rust
pub fn compute_leaf_hash(&self) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let json = serde_json::to_vec(self).expect("serialization cannot fail");

    let mut hasher = Sha256::new();
    hasher.update(&[0x00]);  // RFC 6962 leaf prefix for domain separation
    hasher.update(&json);
    hasher.finalize().into()
}
```

## Signed Tree Head Structure

Message format for signing:

```
tree_size (8 bytes BE) || timestamp (8 bytes BE) || root_hash (32 bytes)
```

Ed25519 signature covers all 48 bytes.

API response format:

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

## Inclusion Proof Protocol

### Proof Generation

For leaf at index `i` in tree of size `n`:

1. Identify sibling nodes needed for path to root
2. Order siblings based on tree traversal direction
3. Package with leaf hash and tree metadata

### Proof Format

```json
{
  "leaf_hash": "abc123...",
  "leaf_index": 5678,
  "tree_size": 12345,
  "proof_hashes": [
    "hash1...", // sibling at level 0
    "hash2..." // sibling at level 1
    // ... up to root
  ],
  "root_hash": "def456..."
}
```

### Verification Algorithm

```rust
pub fn verify_inclusion(
    leaf_hash: &[u8; 32],
    leaf_index: usize,
    proof_hashes: &[[u8; 32]],
    tree_size: usize,
    root_hash: &[u8; 32],
) -> bool {
    let mut hash = *leaf_hash;
    let mut index = leaf_index;

    for sibling in proof_hashes {
        hash = if index % 2 == 0 {
            hash_pair(&hash, sibling)
        } else {
            hash_pair(sibling, &hash)
        };
        index /= 2;
    }

    hash == *root_hash
}

fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&[0x01]);  // RFC 6962 internal node prefix
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}
```

## Batch Processing Strategy

### Trigger Conditions

Commit new STH when ANY condition is met:

- 10 seconds elapsed since last commit
- 100 pending leaves accumulated
- Graceful shutdown initiated

### Processing Pipeline

```rust
async fn commit_pending_leaves(&self) -> Result<Option<SignedTreeHead>> {
    // 1. Lock and drain pending queue
    let pending = self.pending_leaves.write().await.drain(..).collect();

    // 2. Compute leaf hashes
    let leaf_hashes: Vec<[u8; 32]> = pending
        .iter()
        .map(|leaf| leaf.compute_leaf_hash())
        .collect();

    // 3. Update Merkle tree
    let mut tree = self.tree.write().await;
    for hash in &leaf_hashes {
        tree.insert(*hash);
    }
    tree.commit();

    // 4. Sign tree head
    let root = tree.root().context("tree must have root")?;
    let signature = self.signing_service
        .sign_tree_head(&root, tree_size, timestamp)
        .await?;

    // 5. Persist atomically
    let mut tx = self.db.begin().await?;
    insert_leaves(&mut tx, &pending, tree_size).await?;
    insert_sth(&mut tx, tree_size, root, signature).await?;
    tx.commit().await?;

    Ok(Some(sth))
}
```

### Concurrency Model

- Single writer for tree updates (protected by RwLock)
- Multiple readers for proof generation
- Batch accumulation in lock-free queue
- Async processing without blocking delivery

## API Endpoints

### GET /attestation/sth

Returns latest Signed Tree Head.

```rust
async fn fetch_latest_sth(
    State(state): State<AttestationState>,
) -> Result<Json<SignedTreeHeadResponse>, StatusCode> {
    let sth = state.merkle_service
        .latest_sth()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(sth.into()))
}
```

### GET /attestation/proof/:leaf_hash

Generate inclusion proof for specific delivery.

Query parameters:

- `tree_size` (optional) - Historical proof at specific tree size

### GET /attestation/download-proof/:delivery_attempt_id

Complete proof package for offline verification:

```json
{
  "delivery_attempt_id": "da_xyz",
  "leaf_data": {
    /* structured data */
  },
  "leaf_hash": "abc...",
  "inclusion_proof": {
    /* proof hashes */
  },
  "signed_tree_head": {
    /* STH at inclusion */
  },
  "verification_instructions": "Run: kapsel-verify --proof proof.json --key KEY"
}
```

## Client Verification

### Standalone Library

```rust
use kapsel_verification::{ProofVerifier, ProofPackage};

let verifier = ProofVerifier::new(PUBLIC_KEY_HEX)?;
let proof: ProofPackage = serde_json::from_str(&proof_json)?;

match verifier.verify(&proof) {
    Ok(()) => println!("✓ Delivery verified"),
    Err(e) => println!("✗ Verification failed: {}", e),
}
```

### CLI Tool

```bash
# Download proof
curl -O https://api.kapsel.io/attestation/download-proof/da_xyz > proof.json

# Verify independently
kapsel-verify --proof proof.json --public-key 9f8e7d6...

# Output
✓ Proof verified successfully!
  Webhook delivered to https://example.com/webhook
  At timestamp: 2024-01-15T10:30:45Z
  HTTP status: 200
  Cryptographically signed by Kapsel
```

## Security Considerations

### Key Management

**Development**: Ed25519 keys in environment variables
**Production**: HSM or cloud KMS integration required

```rust
// Development
let signing_key = SigningKey::from_bytes(&KEY_BYTES);

// Production
let signing_key = kms_client
    .get_signing_key("kapsel-attestation-key")
    .await?;
```

### Append-Only Enforcement

Consistency proofs verify no tampering:

```rust
pub fn verify_consistency(
    old_size: usize,
    new_size: usize,
    old_root: &[u8; 32],
    new_root: &[u8; 32],
    proof_hashes: &[[u8; 32]],
) -> bool {
    // Verify old tree is prefix of new tree
    // Algorithm per RFC 6962 Section 2.1.2
}
```

### Privacy

- Leaf data includes hashes, not raw payloads
- Tenant isolation through query filtering
- No PII in attestation records

## Performance Optimizations

### Proof Caching

Cache frequently requested proofs:

```rust
async fn fetch_cached_proof(
    &self,
    leaf_hash: &[u8],
    tree_size: i64,
) -> Result<Option<MerkleProof>> {
    // Check cache first
    if let Some(cached) = self.proof_cache.fetch(leaf_hash, tree_size).await? {
        return Ok(Some(cached));
    }

    // Generate and cache
    let proof = self.generate_proof(leaf_hash, tree_size).await?;
    self.proof_cache.insert(leaf_hash, tree_size, &proof).await?;
    Ok(Some(proof))
}
```

### Batch Size Tuning

```toml
[attestation]
batch_timeout_ms = 10000      # 10 seconds
batch_size_threshold = 100    # 100 events
max_batch_size = 1000         # Hard limit
```

### Tree Reconstruction

On startup, rebuild from last STH:

```rust
async fn rebuild_tree(&self) -> Result<()> {
    let last_sth = self.latest_sth().await?;
    let leaves = self.fetch_leaves_up_to(last_sth.tree_size).await?;

    let mut tree = MerkleTree::new();
    for leaf in leaves {
        tree.insert(leaf.leaf_hash);
    }

    assert_eq!(tree.root(), last_sth.root_hash);
    Ok(())
}
```

## Testing Strategy

### Unit Tests

- Leaf hash computation with various payloads
- Merkle tree construction determinism
- Ed25519 signature generation/verification
- Proof generation for all tree sizes

### Property Tests

```rust
proptest! {
    #[test]
    fn all_leaves_provable(
        leaf_count in 1usize..1000
    ) {
        let tree = build_tree(leaf_count);
        for i in 0..leaf_count {
            let proof = tree.proof(&[i]);
            prop_assert!(proof.verify(tree.root(), i, tree.size()));
        }
    }
}
```

### Integration Tests

- End-to-end delivery with attestation capture
- Batch processing under concurrent load
- STH generation timing accuracy
- Client verification with downloaded proofs

## Compliance Benefits

### Use Cases

**Financial Services**

- Prove payment notifications delivered
- Regulatory audit trail for disputes
- SLA verification with timestamps

**Healthcare**

- HIPAA-compliant delivery logs
- Tamper-proof patient notifications
- Chain of custody for sensitive data

**Legal Tech**

- Court-admissible delivery proof
- Contract notification verification
- Dispute resolution evidence

### Regulatory Alignment

- **GDPR Article 5**: Accountability via immutable logs
- **SOC 2 Type II**: Cryptographic integrity controls
- **ISO 27001**: Information security management
- **eIDAS**: Advanced electronic signatures

## Migration Path

### Phase 1: Schema & Foundation (Day 1-2)

- Run migration 002_merkle_attestation.sql
- Deploy MerkleService and SigningService
- Configure batch processing parameters

### Phase 2: Integration (Day 3-4)

- Enable attestation capture in delivery pipeline
- Start background batch worker
- Monitor tree growth and performance

### Phase 3: API & Proofs (Day 5)

- Enable attestation endpoints
- Test proof generation at scale
- Deploy client verification tools

### Phase 4: Production (Week 2)

- Enable for 10% traffic
- Monitor proof generation latency
- Gradual rollout to 100%

## Success Metrics

- **Attestation Latency**: p99 < 100ms batch processing
- **Proof Generation**: p99 < 50ms with caching
- **Tree Growth**: Linear with delivery volume
- **Storage Overhead**: ~500 bytes per delivery
- **Client Verification**: < 10ms with proof package

## Related Documentation

- [System Overview](./OVERVIEW.md) - Overall architecture
- [Technical Specification](./SPECIFICATION.md) - Requirements
- [Testing Strategy](./TESTING_STRATEGY.md) - Test methodology
- [Implementation Status](./IMPLEMENTATION_STATUS.md) - Current progress
