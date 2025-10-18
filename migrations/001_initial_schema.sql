-- Kapsel initial schema - Webhook reliability service with cryptographic attestation
-- This migration creates all tables needed for webhook ingestion, delivery, audit trail,
-- and Merkle tree-based cryptographic attestation of delivery attempts.

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Tenants table - Multi-tenancy support
CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL,

    -- Subscription and limits
    plan TEXT NOT NULL DEFAULT 'free' CHECK (plan IN ('free', 'starter', 'growth', 'scale', 'enterprise')),
    max_events_per_month INTEGER NOT NULL DEFAULT 10000,
    max_endpoints INTEGER NOT NULL DEFAULT 5,
    events_this_month INTEGER NOT NULL DEFAULT 0,

    -- API access
    api_key TEXT NOT NULL UNIQUE DEFAULT encode(gen_random_bytes(32), 'hex'),
    api_key_hash TEXT NOT NULL GENERATED ALWAYS AS (encode(sha256(api_key::bytea), 'hex')) STORED,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,

    -- Billing
    stripe_customer_id TEXT,
    stripe_subscription_id TEXT
);

-- Endpoints table - Webhook destination configuration
CREATE TABLE endpoints (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,

    -- Configuration
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,

    -- Security
    signing_secret TEXT,  -- Encrypted in application
    signature_header TEXT,

    -- Retry policy
    max_retries INTEGER NOT NULL DEFAULT 10 CHECK (max_retries >= 0 AND max_retries <= 100),
    timeout_seconds INTEGER NOT NULL DEFAULT 30 CHECK (timeout_seconds >= 1 AND timeout_seconds <= 300),
    retry_strategy TEXT NOT NULL DEFAULT 'exponential' CHECK (retry_strategy IN ('exponential', 'linear', 'fixed')),

    -- Circuit breaker state (per endpoint)
    circuit_state TEXT NOT NULL DEFAULT 'closed' CHECK (circuit_state IN ('closed', 'open', 'half_open')),
    circuit_failure_count INTEGER NOT NULL DEFAULT 0,
    circuit_success_count INTEGER NOT NULL DEFAULT 0,
    circuit_last_failure_at TIMESTAMPTZ,
    circuit_half_open_at TIMESTAMPTZ,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,

    -- Statistics (denormalized for performance)
    total_events_received BIGINT NOT NULL DEFAULT 0,
    total_events_delivered BIGINT NOT NULL DEFAULT 0,
    total_events_failed BIGINT NOT NULL DEFAULT 0
);

-- Webhook events table - Core event storage
CREATE TABLE webhook_events (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    endpoint_id UUID NOT NULL REFERENCES endpoints(id) ON DELETE CASCADE,

    -- Idempotency
    source_event_id TEXT NOT NULL,  -- Extracted based on strategy
    idempotency_strategy TEXT NOT NULL CHECK (idempotency_strategy IN ('header', 'content', 'source_id')),

    -- Status tracking
    status TEXT NOT NULL DEFAULT 'received' CHECK (
        status IN ('received', 'pending', 'delivering', 'delivered', 'failed', 'dead_letter')
    ),
    failure_count INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TIMESTAMPTZ,
    next_retry_at TIMESTAMPTZ,

    -- Payload (stored as raw bytes)
    headers JSONB NOT NULL,
    body BYTEA NOT NULL,
    content_type TEXT NOT NULL,
    payload_size INTEGER NOT NULL,

    -- Signature validation
    signature_valid BOOLEAN,
    signature_error TEXT,

    -- Timestamps
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,

    -- Prevent duplicate events
    UNIQUE(tenant_id, endpoint_id, source_event_id),

    -- Ensure payload size is reasonable
    CHECK (payload_size > 0 AND payload_size <= 10485760)  -- 10MB max
);

-- Delivery attempts table - Tracks each delivery attempt
CREATE TABLE delivery_attempts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    event_id UUID NOT NULL REFERENCES webhook_events(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),

    -- Request details
    request_url TEXT NOT NULL,
    request_headers JSONB NOT NULL,
    request_method TEXT NOT NULL DEFAULT 'POST',

    -- Response details (null if timeout/network error)
    response_status INTEGER,
    response_headers JSONB,
    response_body TEXT,  -- Truncated to 1KB for debugging

    -- Timing
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    duration_ms INTEGER,

    -- Error tracking
    error_type TEXT CHECK (error_type IN (
        'network', 'timeout', 'dns', 'connection_refused',
        'ssl', 'http_error', 'invalid_response', 'circuit_open'
    )),
    error_message TEXT,

    -- Retry information
    retry_after TIMESTAMPTZ,  -- If server provided Retry-After header

    UNIQUE(event_id, attempt_number)
);

-- API keys table - For API authentication
CREATE TABLE api_keys (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,

    key_hash TEXT NOT NULL UNIQUE,  -- SHA256 hash of the key
    name TEXT NOT NULL,

    -- Permissions (for future RBAC)
    permissions JSONB NOT NULL DEFAULT '["read", "write"]'::jsonb,

    -- Rate limiting
    rate_limit_per_second INTEGER,

    -- Metadata
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at TIMESTAMPTZ,

    CHECK (expires_at IS NULL OR expires_at > created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at)
);

-- Attestation keys table - Ed25519 key storage and rotation
CREATE TABLE attestation_keys (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),

    -- Ed25519 public key (32 bytes)
    public_key BYTEA NOT NULL CHECK (length(public_key) = 32),

    -- Only one key can be active at a time for signing
    is_active BOOLEAN NOT NULL DEFAULT false,

    -- Key metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deactivated_at TIMESTAMPTZ,

    -- Key identification for signature verification
    key_fingerprint TEXT NOT NULL GENERATED ALWAYS AS (
        encode(sha256(public_key), 'hex')
    ) STORED,

    -- Ensure unique public keys
    UNIQUE(public_key),
    CHECK (deactivated_at IS NULL OR deactivated_at >= created_at)
);

-- Merkle leaves table - Individual leaf storage for tree construction
CREATE TABLE merkle_leaves (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),

    -- RFC 6962 compliant leaf hash (32 bytes)
    leaf_hash BYTEA NOT NULL CHECK (length(leaf_hash) = 32),

    -- Reference to delivery attempt this leaf represents
    delivery_attempt_id UUID NOT NULL REFERENCES delivery_attempts(id) ON DELETE CASCADE,

    -- Denormalized fields from delivery attempt for efficient querying
    event_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    endpoint_url TEXT NOT NULL,

    -- SHA256 hash of the webhook payload (32 bytes)
    payload_hash BYTEA NOT NULL CHECK (length(payload_hash) = 32),

    -- Delivery attempt details
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    attempted_at TIMESTAMPTZ NOT NULL,

    -- Tree position (assigned when leaf is added to tree)
    tree_index BIGINT,
    batch_id UUID,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Prevent duplicate leaves for same delivery attempt
    UNIQUE(delivery_attempt_id),

    -- Ensure tree_index is unique when assigned
    UNIQUE(tree_index) DEFERRABLE INITIALLY DEFERRED
);

-- Signed tree heads table - Cryptographically signed Merkle tree states
CREATE TABLE signed_tree_heads (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),

    -- Tree state at time of signing
    tree_size BIGINT NOT NULL CHECK (tree_size >= 0),
    root_hash BYTEA NOT NULL CHECK (length(root_hash) = 32),

    -- Timestamp (milliseconds since Unix epoch)
    timestamp_ms BIGINT NOT NULL CHECK (timestamp_ms > 0),

    -- Ed25519 signature over tree head (64 bytes)
    signature BYTEA NOT NULL CHECK (length(signature) = 64),

    -- Key used for signing
    key_id UUID NOT NULL REFERENCES attestation_keys(id) ON DELETE RESTRICT,

    -- Batch information
    batch_id UUID NOT NULL,
    batch_size INTEGER NOT NULL CHECK (batch_size > 0),

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Ensure tree heads are monotonically increasing
    UNIQUE(tree_size),
    UNIQUE(batch_id)
);

-- Proof cache table - Cached inclusion and consistency proofs
CREATE TABLE proof_cache (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),

    -- Proof type and target
    proof_type TEXT NOT NULL CHECK (proof_type IN ('inclusion', 'consistency')),

    -- For inclusion proofs: leaf index and tree size
    leaf_index BIGINT,
    tree_size BIGINT,

    -- For consistency proofs: old and new tree sizes
    old_tree_size BIGINT,
    new_tree_size BIGINT,

    -- Merkle audit path (array of 32-byte hashes)
    proof_path BYTEA[] NOT NULL,

    -- Cache metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '24 hours'),
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed_at TIMESTAMPTZ,

    -- Constraints based on proof type
    CHECK (
        (proof_type = 'inclusion' AND leaf_index IS NOT NULL AND tree_size IS NOT NULL
         AND old_tree_size IS NULL AND new_tree_size IS NULL)
        OR
        (proof_type = 'consistency' AND old_tree_size IS NOT NULL AND new_tree_size IS NOT NULL
         AND leaf_index IS NULL AND tree_size IS NULL AND new_tree_size > old_tree_size)
    )

    -- Note: Proof path element validation (32 bytes each) handled in application code
    -- PostgreSQL CHECK constraints cannot contain subqueries
);

-- Indexes for performance

-- Tenants indexes - partial unique index for soft deletes
CREATE UNIQUE INDEX idx_tenants_name_active
    ON tenants(name)
    WHERE deleted_at IS NULL;

-- Endpoints indexes - partial unique index for soft deletes
CREATE UNIQUE INDEX idx_endpoints_tenant_name_active
    ON endpoints(tenant_id, name)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_endpoints_tenant
    ON endpoints(tenant_id)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_endpoints_circuit
    ON endpoints(circuit_state)
    WHERE circuit_state != 'closed';

-- Webhook events indexes
CREATE INDEX idx_webhook_events_status
    ON webhook_events(status, next_retry_at)
    WHERE status IN ('pending', 'delivering');

CREATE INDEX idx_webhook_events_tenant
    ON webhook_events(tenant_id, received_at DESC);

CREATE INDEX idx_webhook_events_endpoint
    ON webhook_events(endpoint_id, received_at DESC);

-- Delivery attempts indexes
CREATE INDEX idx_delivery_attempts_event
    ON delivery_attempts(event_id, attempt_number DESC);

CREATE INDEX idx_delivery_attempts_timing
    ON delivery_attempts(attempted_at DESC);

-- API keys indexes
CREATE INDEX idx_api_keys_hash
    ON api_keys(key_hash)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_api_keys_tenant
    ON api_keys(tenant_id)
    WHERE revoked_at IS NULL;

-- Attestation keys indexes
CREATE INDEX idx_attestation_keys_active
    ON attestation_keys(created_at DESC)
    WHERE is_active = true;

CREATE INDEX idx_attestation_keys_fingerprint
    ON attestation_keys(key_fingerprint);

-- Partial unique index to enforce only one active key
CREATE UNIQUE INDEX idx_attestation_keys_single_active
    ON attestation_keys(is_active)
    WHERE is_active = true;

-- Merkle leaves indexes
CREATE INDEX idx_merkle_leaves_tree_index
    ON merkle_leaves(tree_index ASC)
    WHERE tree_index IS NOT NULL;

CREATE INDEX idx_merkle_leaves_batch
    ON merkle_leaves(batch_id, tree_index ASC)
    WHERE batch_id IS NOT NULL;

CREATE INDEX idx_merkle_leaves_tenant_time
    ON merkle_leaves(tenant_id, attempted_at DESC);

CREATE INDEX idx_merkle_leaves_event
    ON merkle_leaves(event_id);

CREATE INDEX idx_merkle_leaves_delivery_attempt
    ON merkle_leaves(delivery_attempt_id);

-- Signed tree heads indexes
CREATE INDEX idx_signed_tree_heads_size_desc
    ON signed_tree_heads(tree_size DESC);

CREATE INDEX idx_signed_tree_heads_timestamp
    ON signed_tree_heads(timestamp_ms DESC);

CREATE INDEX idx_signed_tree_heads_batch
    ON signed_tree_heads(batch_id);

CREATE INDEX idx_signed_tree_heads_key
    ON signed_tree_heads(key_id);

-- Proof cache indexes
CREATE INDEX idx_proof_cache_inclusion
    ON proof_cache(leaf_index, tree_size)
    WHERE proof_type = 'inclusion';

CREATE INDEX idx_proof_cache_consistency
    ON proof_cache(old_tree_size, new_tree_size)
    WHERE proof_type = 'consistency';

CREATE INDEX idx_proof_cache_expiry
    ON proof_cache(expires_at);

-- Functions and triggers

-- Function to update updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Apply updated_at trigger to tables
CREATE TRIGGER update_tenants_updated_at BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_endpoints_updated_at BEFORE UPDATE ON endpoints
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Function to check tenant limits
CREATE OR REPLACE FUNCTION check_tenant_limits()
RETURNS TRIGGER AS $$
DECLARE
    current_endpoints INTEGER;
    max_allowed INTEGER;
BEGIN
    IF TG_TABLE_NAME = 'endpoints' THEN
        SELECT COUNT(*), t.max_endpoints
        INTO current_endpoints, max_allowed
        FROM endpoints e
        JOIN tenants t ON t.id = NEW.tenant_id
        WHERE e.tenant_id = NEW.tenant_id
            AND e.deleted_at IS NULL
        GROUP BY t.max_endpoints;

        IF current_endpoints >= max_allowed THEN
            RAISE EXCEPTION 'Endpoint limit exceeded for tenant %', NEW.tenant_id;
        END IF;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER enforce_endpoint_limits BEFORE INSERT ON endpoints
    FOR EACH ROW EXECUTE FUNCTION check_tenant_limits();

-- Function to clean expired proof cache entries
CREATE OR REPLACE FUNCTION clean_expired_proof_cache()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM proof_cache WHERE expires_at <= NOW();
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- Function to update proof cache access statistics
CREATE OR REPLACE FUNCTION update_proof_cache_access()
RETURNS TRIGGER AS $$
BEGIN
    NEW.access_count = OLD.access_count + 1;
    NEW.last_accessed_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_proof_cache_access_trigger
    BEFORE UPDATE ON proof_cache
    FOR EACH ROW
    WHEN (OLD.access_count IS DISTINCT FROM NEW.access_count)
    EXECUTE FUNCTION update_proof_cache_access();

-- Initial data

-- Create default system tenant for internal use
INSERT INTO tenants (id, name, plan, max_events_per_month, max_endpoints)
VALUES (
    '00000000-0000-0000-0000-000000000000'::uuid,
    'System',
    'enterprise',
    2147483647,  -- Max int
    2147483647
) ON CONFLICT DO NOTHING;

-- Comments for documentation

COMMENT ON TABLE webhook_events IS 'Core table storing all received webhook events and their delivery status';
COMMENT ON COLUMN webhook_events.status IS 'Event lifecycle: received -> pending -> delivering -> delivered/failed';
COMMENT ON COLUMN webhook_events.source_event_id IS 'Unique identifier from source system for idempotency';

COMMENT ON TABLE endpoints IS 'Webhook destination configuration with circuit breaker state';
COMMENT ON COLUMN endpoints.circuit_state IS 'Circuit breaker pattern: closed (normal), open (failing), half_open (testing recovery)';

COMMENT ON TABLE delivery_attempts IS 'Audit trail of every delivery attempt with full request/response details';
COMMENT ON TABLE tenants IS 'Multi-tenant support with subscription plans and usage limits';

COMMENT ON TABLE attestation_keys IS 'Ed25519 keys for signing Merkle tree heads with rotation support';
COMMENT ON COLUMN attestation_keys.public_key IS 'Ed25519 public key (32 bytes) for signature verification';
COMMENT ON COLUMN attestation_keys.is_active IS 'Only one key can be active for signing at any time';
COMMENT ON COLUMN attestation_keys.key_fingerprint IS 'SHA256 fingerprint for key identification';

COMMENT ON TABLE merkle_leaves IS 'Individual Merkle tree leaves representing webhook delivery attempts';
COMMENT ON COLUMN merkle_leaves.leaf_hash IS 'RFC 6962 compliant leaf hash (32 bytes)';
COMMENT ON COLUMN merkle_leaves.tree_index IS 'Position in Merkle tree when assigned to batch';
COMMENT ON COLUMN merkle_leaves.payload_hash IS 'SHA256 hash of webhook payload for integrity verification';

COMMENT ON TABLE signed_tree_heads IS 'Cryptographically signed Merkle tree states for tamper evidence';
COMMENT ON COLUMN signed_tree_heads.tree_size IS 'Number of leaves in tree at time of signing';
COMMENT ON COLUMN signed_tree_heads.root_hash IS 'Merkle tree root hash (32 bytes)';
COMMENT ON COLUMN signed_tree_heads.timestamp_ms IS 'Unix timestamp in milliseconds';
COMMENT ON COLUMN signed_tree_heads.signature IS 'Ed25519 signature over tree head (64 bytes)';

COMMENT ON TABLE proof_cache IS 'Cached Merkle proofs for efficient verification';
COMMENT ON COLUMN proof_cache.proof_type IS 'Type of proof: inclusion (leaf membership) or consistency (tree extension)';
COMMENT ON COLUMN proof_cache.proof_path IS 'Array of 32-byte hashes forming the Merkle audit path';
