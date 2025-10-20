-- Migration: Remove raw API key storage from tenants table
--
-- Security fix: Raw API keys should never be stored in the database.
-- The api_keys table with hashed keys is the single source of truth for authentication.
--
-- This migration removes the vulnerable api_key and api_key_hash columns from the tenants table.

-- Drop the generated column first (depends on api_key)
ALTER TABLE tenants DROP COLUMN IF EXISTS api_key_hash;

-- Drop the raw API key column (SECURITY: never store raw secrets)
ALTER TABLE tenants DROP COLUMN IF EXISTS api_key;

-- Add comment to document the security decision
COMMENT ON TABLE tenants IS 'Multi-tenant configuration. API keys are stored separately in api_keys table with proper hashing for security.';
COMMENT ON TABLE api_keys IS 'API authentication keys. Only stores SHA256 hashes of keys, never raw values. Keys are shown once at generation and must be stored securely by the user.';
