-- Initial schema for Hooky webhook reliability service
-- This migration creates the core tables needed for webhook ingestion,
-- delivery, and audit trail.

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ============================================================================
-- Tenants table - Multi-tenancy support
-- ============================================================================
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
    stripe_subscription_id TEXT,

    UNIQUE(name) WHERE deleted_at IS NULL
);

-- ============================================================================
-- Endpoints table - Webhook destination configuration
-- ============================================================================
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
    total_events_failed BIGINT NOT NULL DEFAULT 0,

    UNIQUE(tenant_id, name) WHERE deleted_at IS NULL
);

-- ============================================================================
-- Webhook events table - Core event storage
-- ============================================================================
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

    -- Audit trail reference
    tigerbeetle_id UUID,  -- Reference to immutable log entry

    -- Prevent duplicate events
    UNIQUE(tenant_id, endpoint_id, source_event_id),

    -- Ensure payload size is reasonable
    CHECK (payload_size > 0 AND payload_size <= 10485760)  -- 10MB max
);

-- ============================================================================
-- Delivery attempts table - Tracks each delivery attempt
-- ============================================================================
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

-- ============================================================================
-- API keys table - For API authentication
-- ============================================================================
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

-- ============================================================================
-- Indexes for performance
-- ============================================================================

-- Webhook events indexes
CREATE INDEX idx_webhook_events_status
    ON webhook_events(status, next_retry_at)
    WHERE status IN ('pending', 'delivering');

CREATE INDEX idx_webhook_events_tenant
    ON webhook_events(tenant_id, received_at DESC);

CREATE INDEX idx_webhook_events_endpoint
    ON webhook_events(endpoint_id, received_at DESC);

CREATE INDEX idx_webhook_events_tigerbeetle
    ON webhook_events(tigerbeetle_id)
    WHERE tigerbeetle_id IS NULL;

-- Delivery attempts indexes
CREATE INDEX idx_delivery_attempts_event
    ON delivery_attempts(event_id, attempt_number DESC);

CREATE INDEX idx_delivery_attempts_timing
    ON delivery_attempts(attempted_at DESC);

-- Endpoints indexes
CREATE INDEX idx_endpoints_tenant
    ON endpoints(tenant_id)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_endpoints_circuit
    ON endpoints(circuit_state)
    WHERE circuit_state != 'closed';

-- API keys indexes
CREATE INDEX idx_api_keys_hash
    ON api_keys(key_hash)
    WHERE revoked_at IS NULL;

CREATE INDEX idx_api_keys_tenant
    ON api_keys(tenant_id)
    WHERE revoked_at IS NULL;

-- ============================================================================
-- Functions and triggers
-- ============================================================================

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

-- ============================================================================
-- Initial data
-- ============================================================================

-- Create default system tenant for internal use
INSERT INTO tenants (id, name, plan, max_events_per_month, max_endpoints)
VALUES (
    '00000000-0000-0000-0000-000000000000'::uuid,
    'System',
    'enterprise',
    2147483647,  -- Max int
    2147483647
) ON CONFLICT DO NOTHING;

-- ============================================================================
-- Comments for documentation
-- ============================================================================

COMMENT ON TABLE webhook_events IS 'Core table storing all received webhook events and their delivery status';
COMMENT ON COLUMN webhook_events.status IS 'Event lifecycle: received -> pending -> delivering -> delivered/failed';
COMMENT ON COLUMN webhook_events.source_event_id IS 'Unique identifier from source system for idempotency';
COMMENT ON COLUMN webhook_events.tigerbeetle_id IS 'Reference to immutable audit log entry in TigerBeetle';

COMMENT ON TABLE endpoints IS 'Webhook destination configuration with circuit breaker state';
COMMENT ON COLUMN endpoints.circuit_state IS 'Circuit breaker pattern: closed (normal), open (failing), half_open (testing recovery)';

COMMENT ON TABLE delivery_attempts IS 'Audit trail of every delivery attempt with full request/response details';
COMMENT ON TABLE tenants IS 'Multi-tenant support with subscription plans and usage limits';
