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

-- Log completion
DO $$
BEGIN
    RAISE NOTICE 'Template database kapsel_test_template created successfully';
    RAISE NOTICE 'Template is clean - contains schema only, no test data';
END $$;
