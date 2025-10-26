-- Migration 004: Align database schema with Rust models
--
-- This migration addresses discrepancies between the database schema and Rust models
-- to ensure consistency across the codebase and prevent runtime errors.
--
-- Changes:
-- 1. Rename tenants.plan to tenants.tier for consistency with model naming
-- 2. Add endpoint_id to delivery_attempts for direct endpoint reference
-- 3. Simplify delivery_attempts by removing redundant columns (already in webhook_events)
-- 4. Fix idempotency_strategy values ('content' -> 'content_hash')
-- 5. Consolidate signing_secret/signature_header into signature_config column

-- 1. Rename plan column to tier in tenants table
ALTER TABLE tenants RENAME COLUMN plan TO tier;

-- Update the check constraint for tier values
ALTER TABLE tenants DROP CONSTRAINT IF EXISTS tenants_plan_check;
ALTER TABLE tenants ADD CONSTRAINT tenants_tier_check
    CHECK (tier IN ('free', 'starter', 'growth', 'scale', 'enterprise', 'system'));

-- 2. Add endpoint_id to delivery_attempts table
-- This allows direct querying of attempts by endpoint without joining through events
ALTER TABLE delivery_attempts
    ADD COLUMN endpoint_id UUID REFERENCES endpoints(id) ON DELETE CASCADE;

-- Populate endpoint_id from existing data
UPDATE delivery_attempts da
SET endpoint_id = we.endpoint_id
FROM webhook_events we
WHERE da.event_id = we.id;

-- Make endpoint_id NOT NULL after population
ALTER TABLE delivery_attempts
    ALTER COLUMN endpoint_id SET NOT NULL;

-- Create index for efficient endpoint-based queries
CREATE INDEX idx_delivery_attempts_endpoint_id ON delivery_attempts(endpoint_id);
CREATE INDEX idx_delivery_attempts_endpoint_attempted_at ON delivery_attempts(endpoint_id, attempted_at DESC);

-- 3. Simplify delivery_attempts schema
-- Remove columns that duplicate information already in webhook_events
ALTER TABLE delivery_attempts
    DROP COLUMN IF EXISTS request_url,
    DROP COLUMN IF EXISTS request_method,
    DROP COLUMN IF EXISTS duration_ms,
    DROP COLUMN IF EXISTS error_type;

-- Add columns to match Rust model
ALTER TABLE delivery_attempts
    ADD COLUMN IF NOT EXISTS request_body BYTEA,
    ADD COLUMN IF NOT EXISTS succeeded BOOLEAN NOT NULL DEFAULT false;

-- Update response_body to BYTEA for consistency with request_body
ALTER TABLE delivery_attempts
    ALTER COLUMN response_body TYPE BYTEA USING response_body::bytea;

-- 4. Fix idempotency_strategy values to match Rust enum
UPDATE webhook_events
SET idempotency_strategy = 'content_hash'
WHERE idempotency_strategy = 'content';

-- Update the check constraint for idempotency_strategy
ALTER TABLE webhook_events DROP CONSTRAINT IF EXISTS webhook_events_idempotency_strategy_check;
ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_idempotency_strategy_check
    CHECK (idempotency_strategy IN ('header', 'source_id', 'content_hash'));

-- 5. Consolidate signature configuration into single column
-- Add signature_config column to endpoints table
ALTER TABLE endpoints
ADD COLUMN signature_config TEXT NOT NULL DEFAULT 'none';

-- Migrate existing signature data to the new format
-- Format: "none" | "hmac_sha256:header:secret"
UPDATE endpoints
SET signature_config = CASE
    WHEN signing_secret IS NULL OR signing_secret = '' THEN 'none'
    ELSE 'hmac_sha256:' || COALESCE(signature_header, 'X-Webhook-Signature') || ':' || signing_secret
END;

-- Drop the old signature columns
ALTER TABLE endpoints DROP COLUMN IF EXISTS signing_secret;
ALTER TABLE endpoints DROP COLUMN IF EXISTS signature_header;

-- 6. Update system tenant to use correct name and tier
UPDATE tenants
SET name = 'system', tier = 'system'
WHERE id = '00000000-0000-0000-0000-000000000000'::uuid;

-- 7. Add helpful indexes for common query patterns
CREATE INDEX IF NOT EXISTS idx_webhook_events_tenant_endpoint_status
    ON webhook_events(tenant_id, endpoint_id, status)
    WHERE status IN ('pending', 'delivering');

CREATE INDEX IF NOT EXISTS idx_webhook_events_next_retry
    ON webhook_events(next_retry_at)
    WHERE status = 'pending' AND next_retry_at IS NOT NULL;

-- 8. Data integrity checks
-- Verify idempotency_strategy values are correct
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM webhook_events
        WHERE idempotency_strategy NOT IN ('header', 'source_id', 'content_hash')
    ) THEN
        RAISE EXCEPTION 'Invalid idempotency_strategy values found after migration';
    END IF;
END $$;

-- Verify endpoints signature_config is properly formatted
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM endpoints
        WHERE signature_config != 'none'
          AND signature_config !~ '^hmac_sha256:[^:]+:.+'
    ) THEN
        RAISE EXCEPTION 'Invalid signature_config values found after migration';
    END IF;
END $$;

-- Update table comments to reflect changes
COMMENT ON COLUMN tenants.tier IS 'Subscription tier determining feature access and limits';
COMMENT ON COLUMN delivery_attempts.endpoint_id IS 'Direct reference to target endpoint for efficient queries';
COMMENT ON COLUMN delivery_attempts.request_body IS 'Complete request payload sent to endpoint';
COMMENT ON COLUMN delivery_attempts.succeeded IS 'Whether this delivery attempt was successful';
COMMENT ON COLUMN delivery_attempts.response_body IS 'Response payload received from endpoint (may be truncated)';
COMMENT ON COLUMN endpoints.signature_config IS 'Signature configuration: "none" or "hmac_sha256:header:secret"';
