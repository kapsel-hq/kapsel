#!/usr/bin/env bash

set -euo pipefail

# Database State Inspection Script
#
# This script provides real-time monitoring and inspection of the PostgreSQL
# test database state to help diagnose database proliferation issues.

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
readonly CONTAINER_NAME="kapsel-postgres-test"
readonly TEST_DB_PORT="5433"
readonly POSTGRES_USER="postgres"
readonly POSTGRES_PASSWORD="postgres"

# Colors for output
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly CYAN='\033[0;36m'
readonly BOLD='\033[1m'
readonly NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

log_header() {
    echo -e "${BOLD}${CYAN}$*${NC}"
}

# Check if container is running
check_container_status() {
    if ! docker ps -q -f name="^${CONTAINER_NAME}$" | grep -q .; then
        log_error "PostgreSQL test container '${CONTAINER_NAME}' is not running"
        log_info "Start it with: cargo make db-start"
        log_info "Or run fresh container test: cargo make test-fresh-container"
        return 1
    fi

    if ! docker exec "${CONTAINER_NAME}" pg_isready -U "${POSTGRES_USER}" >/dev/null 2>&1; then
        log_error "PostgreSQL is not ready in container '${CONTAINER_NAME}'"
        return 1
    fi

    log_success "PostgreSQL test container is running and ready"
    return 0
}

# Get detailed database information
show_database_overview() {
    log_header "=== DATABASE OVERVIEW ==="
    echo ""

    # Database list with sizes
    echo "Databases with sizes:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            pg_size_pretty(pg_database_size(datname)) as \"Size\",
            datconnlimit as \"Conn Limit\",
            CASE
                WHEN datname LIKE 'test_%' THEN 'Test DB'
                WHEN datname LIKE 'kapsel_template%' THEN 'Template'
                WHEN datname IN ('postgres', 'template0', 'template1') THEN 'System'
                ELSE 'Other'
            END as \"Type\"
         FROM pg_database
         WHERE datistemplate = false
         ORDER BY
            CASE
                WHEN datname LIKE 'kapsel_template%' THEN 1
                WHEN datname LIKE 'test_%' THEN 2
                ELSE 3
            END,
            datname;" 2>/dev/null

    echo ""
}

# Show database counts and patterns
show_database_counts() {
    log_header "=== DATABASE COUNTS ==="
    echo ""

    local total=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT COUNT(*) FROM pg_database WHERE datistemplate = false;" 2>/dev/null | tr -d ' ')

    local test_dbs=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT COUNT(*) FROM pg_database WHERE datname LIKE 'test_%';" 2>/dev/null | tr -d ' ')

    local template_dbs=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT COUNT(*) FROM pg_database WHERE datname LIKE 'kapsel_template%';" 2>/dev/null | tr -d ' ')

    local system_dbs=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT COUNT(*) FROM pg_database WHERE datname IN ('postgres', 'kapsel_test');" 2>/dev/null | tr -d ' ')

    echo "Total databases:    ${total}"
    echo "├── Test databases: ${test_dbs}"
    echo "├── Templates:      ${template_dbs}"
    echo "└── System/Base:    ${system_dbs}"
    echo ""

    if [[ ${test_dbs} -gt 0 ]]; then
        log_warn "Found ${test_dbs} test databases - these should be cleaned up automatically"
        echo "Recent test database names:"
        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
            "SELECT '  - ' || datname FROM pg_database WHERE datname LIKE 'test_%' ORDER BY datname LIMIT 10;" 2>/dev/null
        echo ""
    fi

    if [[ ${template_dbs} -gt 1 ]]; then
        log_warn "Found ${template_dbs} template databases - should only be 1 (kapsel_template)"
        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
            "SELECT '  - ' || datname FROM pg_database WHERE datname LIKE 'kapsel_template%' ORDER BY datname;" 2>/dev/null
        echo ""
    fi
}

# Show active connections
show_connections() {
    log_header "=== ACTIVE CONNECTIONS ==="
    echo ""

    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            count(*) as \"Connections\",
            string_agg(DISTINCT state, ', ') as \"States\",
            string_agg(DISTINCT application_name, ', ') as \"Applications\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
         GROUP BY datname
         ORDER BY count(*) DESC;" 2>/dev/null

    echo ""

    local total_connections=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT count(*) FROM pg_stat_activity WHERE datname IS NOT NULL;" 2>/dev/null | tr -d ' ')

    local max_connections=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SHOW max_connections;" 2>/dev/null | tr -d ' ')

    echo "Connection usage: ${total_connections}/${max_connections} ($(( total_connections * 100 / max_connections ))%)"
    echo ""
}

# Show resource usage
show_resource_usage() {
    log_header "=== RESOURCE USAGE ==="
    echo ""

    # PostgreSQL settings
    echo "PostgreSQL Configuration:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT name, setting, unit, short_desc
         FROM pg_settings
         WHERE name IN (
            'shared_buffers', 'max_connections', 'work_mem',
            'maintenance_work_mem', 'effective_cache_size'
         )
         ORDER BY name;" 2>/dev/null

    echo ""

    # Database sizes
    echo "Total database cluster size:"
    local total_size=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT pg_size_pretty(sum(pg_database_size(datname))) FROM pg_database;" 2>/dev/null | tr -d ' ')
    echo "  ${total_size}"

    echo ""

    # Container stats
    echo "Container resource usage:"
    docker stats "${CONTAINER_NAME}" --no-stream --format \
        "  CPU: {{.CPUPerc}}\n  Memory: {{.MemUsage}}\n  Network: {{.NetIO}}\n  Block I/O: {{.BlockIO}}" 2>/dev/null

    echo ""
}

# Show database activity and locks
show_database_activity() {
    log_header "=== DATABASE ACTIVITY ==="
    echo ""

    # Recent queries (last 10)
    echo "Recent query activity:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            usename as \"User\",
            application_name as \"App\",
            state,
            substring(query, 1, 60) || '...' as \"Query\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
           AND query NOT LIKE '%pg_stat_activity%'
           AND state != 'idle'
         ORDER BY query_start DESC
         LIMIT 10;" 2>/dev/null

    echo ""

    # Locks if any
    local lock_count=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT count(*) FROM pg_locks WHERE NOT granted;" 2>/dev/null | tr -d ' ')

    if [[ ${lock_count} -gt 0 ]]; then
        log_warn "Found ${lock_count} ungranted locks"
        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
            "SELECT datname, locktype, mode, granted
             FROM pg_locks l
             LEFT JOIN pg_database d ON d.oid = l.database
             WHERE NOT granted;" 2>/dev/null
        echo ""
    else
        echo "No blocking locks detected"
        echo ""
    fi
}

# Cleanup orphaned databases
cleanup_orphaned_databases() {
    log_info "Scanning for orphaned databases to cleanup..."

    local test_dbs=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT string_agg(datname, ' ') FROM pg_database WHERE datname LIKE 'test_%';" 2>/dev/null | tr -d '\n')

    local old_templates=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT string_agg(datname, ' ') FROM pg_database WHERE datname LIKE 'kapsel_template_%';" 2>/dev/null | tr -d '\n')

    if [[ -n "${test_dbs// /}" ]] || [[ -n "${old_templates// /}" ]]; then
        log_warn "Found orphaned databases that can be cleaned up:"

        if [[ -n "${test_dbs// /}" ]]; then
            echo "  Test databases: ${test_dbs}"
        fi

        if [[ -n "${old_templates// /}" ]]; then
            echo "  Old templates: ${old_templates}"
        fi

        echo ""
        read -p "Clean up these databases? [y/N]: " -n 1 -r
        echo ""

        if [[ $REPLY =~ ^[Yy]$ ]]; then
            log_info "Cleaning up orphaned databases..."

            # Terminate connections and drop databases
            if [[ -n "${test_dbs// /}" ]]; then
                for db in ${test_dbs}; do
                    if [[ -n "${db// /}" ]]; then
                        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
                            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '${db}';" >/dev/null 2>&1
                        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
                            "DROP DATABASE IF EXISTS \"${db}\";" >/dev/null 2>&1
                        log_info "Cleaned up test database: ${db}"
                    fi
                done
            fi

            if [[ -n "${old_templates// /}" ]]; then
                for db in ${old_templates}; do
                    if [[ -n "${db// /}" ]]; then
                        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
                            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '${db}';" >/dev/null 2>&1
                        docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
                            "DROP DATABASE IF EXISTS \"${db}\";" >/dev/null 2>&1
                        log_info "Cleaned up old template: ${db}"
                    fi
                done
            fi

            log_success "Cleanup completed"
        else
            log_info "Cleanup cancelled"
        fi
    else
        log_success "No orphaned databases found"
    fi
    echo ""
}

# Watch mode - continuous monitoring
watch_database_state() {
    log_info "Starting continuous database monitoring (Ctrl+C to exit)..."
    echo ""

    while true; do
        clear
        echo "$(date '+%Y-%m-%d %H:%M:%S') - PostgreSQL Database State Monitor"
        echo "Container: ${CONTAINER_NAME} | Port: ${TEST_DB_PORT}"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""

        show_database_counts
        show_connections

        echo "Press Ctrl+C to exit, or wait for next update in 3 seconds..."
        sleep 3 || break
    done
}

# Display full inspection report
full_inspection() {
    local timestamp=$(date '+%Y-%m-%d %H:%M:%S')
    echo "PostgreSQL Database State Inspection - ${timestamp}"
    echo "Container: ${CONTAINER_NAME} | Port: ${TEST_DB_PORT}"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    show_database_overview
    show_database_counts
    show_connections
    show_resource_usage
    show_database_activity
}

# Script usage information
usage() {
    cat << EOF
Database State Inspection Tool

Usage: $0 [COMMAND]

Commands:
  overview, o     Show database overview (default)
  counts, c       Show database counts by type
  connections     Show active connections
  resources, r    Show resource usage
  activity, a     Show database activity and locks
  cleanup         Clean up orphaned test databases
  watch, w        Continuous monitoring mode
  full, f         Full detailed inspection
  help, h         Show this help

Examples:
  $0               # Show overview
  $0 watch         # Continuous monitoring
  $0 cleanup       # Clean up orphaned databases
  $0 full          # Full detailed report

This tool helps monitor and diagnose database proliferation issues
in the PostgreSQL test container.

EOF
}

# Main execution
main() {
    local command="${1:-overview}"

    if ! check_container_status; then
        exit 1
    fi

    case "$command" in
        overview|o)
            show_database_overview
            show_database_counts
            ;;
        counts|c)
            show_database_counts
            ;;
        connections)
            show_connections
            ;;
        resources|r)
            show_resource_usage
            ;;
        activity|a)
            show_database_activity
            ;;
        cleanup)
            cleanup_orphaned_databases
            ;;
        watch|w)
            watch_database_state
            ;;
        full|f)
            full_inspection
            ;;
        help|h)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown command: $command"
            usage
            exit 1
            ;;
    esac
}

# Handle Ctrl+C gracefully in watch mode
trap 'echo -e "\n${GREEN}Monitoring stopped.${NC}"; exit 0' INT

# Execute main function
main "$@"
