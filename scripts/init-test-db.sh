#!/usr/bin/env bash
#
# Initialize PostgreSQL test database for Kapsel
#
# This script ensures the test database is running and migrations are applied.
# It's idempotent - safe to run multiple times.

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
CONTAINER_NAME="kapsel-postgres-test"
DB_PORT="5433"
DB_USER="postgres"
DB_PASSWORD="postgres"
DB_NAME="kapsel_test"
DATABASE_URL="postgresql://${DB_USER}:${DB_PASSWORD}@localhost:${DB_PORT}/${DB_NAME}"

# Functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

check_docker() {
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        exit 1
    fi

    if ! docker info &> /dev/null; then
        log_error "Docker daemon is not running"
        exit 1
    fi
}

check_sqlx_cli() {
    if ! command -v sqlx &> /dev/null; then
        log_warn "sqlx-cli not found, attempting to install..."
        cargo install sqlx-cli --version=0.8.6 --features=rustls,postgres --no-default-features
    fi
}

start_postgres_test() {
    local container_state=$(docker inspect -f '{{.State.Status}}' $CONTAINER_NAME 2>/dev/null || echo "not_found")

    case "$container_state" in
        "running")
            log_info "PostgreSQL test container is already running"
            ;;
        "exited"|"paused"|"restarting")
            log_info "Starting existing PostgreSQL test container..."
            docker start $CONTAINER_NAME
            ;;
        "not_found")
            log_info "Creating PostgreSQL test container..."
            docker run -d \
                --name $CONTAINER_NAME \
                -e POSTGRES_USER=$DB_USER \
                -e POSTGRES_PASSWORD=$DB_PASSWORD \
                -e POSTGRES_DB=$DB_NAME \
                -p ${DB_PORT}:5432 \
                --tmpfs /var/lib/postgresql/data:rw,size=2g \
                --health-cmd="pg_isready -U $DB_USER" \
                --health-interval=5s \
                --health-timeout=5s \
                --health-retries=5 \
                postgres:16-alpine
            ;;
        *)
            log_error "Unknown container state: $container_state"
            exit 1
            ;;
    esac
}

wait_for_postgres() {
    log_info "Waiting for PostgreSQL to be ready..."
    local max_attempts=30
    local attempt=0

    while [ $attempt -lt $max_attempts ]; do
        if docker exec $CONTAINER_NAME pg_isready -U $DB_USER &>/dev/null; then
            log_info "PostgreSQL is ready!"
            return 0
        fi

        echo -n "."
        sleep 1
        ((attempt++))
    done

    echo ""
    log_error "PostgreSQL failed to become ready after ${max_attempts} seconds"
    return 1
}

run_migrations() {
    log_info "Running database migrations..."

    # Export for sqlx
    export DATABASE_URL

    # Find migrations directory
    local migrations_dir=""
    if [ -d "migrations" ]; then
        migrations_dir="migrations"
    elif [ -d "../migrations" ]; then
        migrations_dir="../migrations"
    elif [ -d "../../migrations" ]; then
        migrations_dir="../../migrations"
    else
        log_error "Could not find migrations directory"
        exit 1
    fi

    if sqlx migrate run --source "$migrations_dir"; then
        log_info "Migrations completed successfully"
    else
        log_error "Migration failed"
        exit 1
    fi
}

verify_setup() {
    log_info "Verifying database setup..."

    # Test connection
    if docker exec $CONTAINER_NAME psql -U $DB_USER -d $DB_NAME -c "SELECT 1" &>/dev/null; then
        log_info "Database connection verified"
    else
        log_error "Failed to connect to database"
        return 1
    fi

    # Check if migrations were applied (check for at least one table)
    local table_count=$(docker exec $CONTAINER_NAME psql -U $DB_USER -d $DB_NAME -t -c "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = 'public'" | xargs)

    if [ "$table_count" -gt 0 ]; then
        log_info "Found $table_count tables - migrations appear to be applied"
    else
        log_warn "No tables found - migrations may not have been applied"
    fi
}

print_usage() {
    echo ""
    echo "Test database is ready!"
    echo ""
    echo "Connection details:"
    echo "  DATABASE_URL: $DATABASE_URL"
    echo "  Host: localhost"
    echo "  Port: $DB_PORT"
    echo "  Database: $DB_NAME"
    echo "  User: $DB_USER"
    echo "  Password: $DB_PASSWORD"
    echo ""
    echo "To run tests:"
    echo "  cargo test"
    echo "  cargo make test"
    echo "  cargo nextest run"
    echo ""
    echo "To stop the container:"
    echo "  docker stop $CONTAINER_NAME"
    echo ""
    echo "To remove the container:"
    echo "  docker rm -f $CONTAINER_NAME"
}

main() {
    echo "=== Kapsel Test Database Initialization ==="
    echo ""

    # Check prerequisites
    check_docker
    check_sqlx_cli

    # Start or create container
    start_postgres_test

    # Wait for database
    if ! wait_for_postgres; then
        log_error "Failed to initialize test database"
        exit 1
    fi

    # Run migrations
    run_migrations

    # Verify setup
    verify_setup

    # Print usage information
    print_usage
}

# Handle script arguments
case "${1:-}" in
    stop)
        log_info "Stopping test database..."
        docker stop $CONTAINER_NAME
        ;;
    clean)
        log_info "Removing test database container..."
        docker rm -f $CONTAINER_NAME
        ;;
    status)
        container_state=$(docker inspect -f '{{.State.Status}}' $CONTAINER_NAME 2>/dev/null || echo "not found")
        echo "Container status: $container_state"
        ;;
    *)
        main
        ;;
esac
