#!/bin/bash
set -e

# Create template database for fast test isolation
# This script runs during Docker container initialization

echo "Creating kapsel_test_template database..."

# Create template from main database (copies schema + any existing data)
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
    -- Drop existing template if it exists
    DROP DATABASE IF EXISTS kapsel_test_template;

    -- Create template from main database
    CREATE DATABASE kapsel_test_template WITH TEMPLATE kapsel_test;
EOSQL

echo "Template database kapsel_test_template created successfully"
echo "Template contains schema only - ready for fast test isolation"
