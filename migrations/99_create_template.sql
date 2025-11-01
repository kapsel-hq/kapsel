-- Create clean template database for fast test isolation
-- Copy schema from main database, then remove all test data

-- Terminate any existing connections to template database
SELECT pg_terminate_backend(pid)
FROM pg_stat_activity
WHERE datname = 'kapsel_test_template' AND pid <> pg_backend_pid();

-- Drop template database if it exists
DROP DATABASE IF EXISTS kapsel_test_template;

-- Create template from main database (copies schema + any existing data)
CREATE DATABASE kapsel_test_template WITH TEMPLATE kapsel_test;

-- Connect to template database to clean out test data
\c kapsel_test_template

-- Truncate all user tables to remove test data, preserving schema
-- Keep _sqlx_migrations intact for version tracking
TRUNCATE TABLE webhook_events RESTART IDENTITY CASCADE;
TRUNCATE TABLE delivery_attempts RESTART IDENTITY CASCADE;
TRUNCATE TABLE merkle_leaves RESTART IDENTITY CASCADE;
TRUNCATE TABLE signed_tree_heads RESTART IDENTITY CASCADE;
TRUNCATE TABLE proof_cache RESTART IDENTITY CASCADE;
TRUNCATE TABLE endpoints RESTART IDENTITY CASCADE;
TRUNCATE TABLE api_keys RESTART IDENTITY CASCADE;
TRUNCATE TABLE tenants RESTART IDENTITY CASCADE;
TRUNCATE TABLE attestation_keys RESTART IDENTITY CASCADE;

-- Switch back to postgres database
\c postgres

-- Log completion
DO $$
BEGIN
    RAISE NOTICE 'Template database kapsel_test_template created successfully';
    RAISE NOTICE 'Template is clean - contains schema only, no test data';
END $$;
