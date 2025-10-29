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

# Show detailed connection analysis for debugging
show_connection_details() {
    log_header "=== CONNECTION POOL ANALYSIS ==="
    echo ""

    echo "Connection states and timing:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            state,
            count(*) as \"Count\",
            application_name as \"App\",
            CASE
                WHEN state_change IS NOT NULL THEN
                    EXTRACT(EPOCH FROM (now() - state_change))::int || 's'
                ELSE 'unknown'
            END as \"State Duration\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
         GROUP BY datname, state, application_name, state_change
         ORDER BY datname, count(*) DESC;" 2>/dev/null

    echo ""

    echo "Long-running connections (>30s in same state):"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            pid,
            state,
            application_name as \"App\",
            EXTRACT(EPOCH FROM (now() - state_change))::int as \"Duration (s)\",
            CASE
                WHEN query_start IS NOT NULL THEN
                    substring(query, 1, 80) || '...'
                ELSE 'No query'
            END as \"Current Query\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
           AND state_change < now() - interval '30 seconds'
         ORDER BY state_change;" 2>/dev/null

    echo ""

    echo "Connection acquisition patterns:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            application_name as \"Application\",
            count(*) as \"Total Conns\",
            count(*) FILTER (WHERE state = 'active') as \"Active\",
            count(*) FILTER (WHERE state = 'idle') as \"Idle\",
            count(*) FILTER (WHERE state = 'idle in transaction') as \"Idle in TX\",
            count(*) FILTER (WHERE state = 'idle in transaction (aborted)') as \"Aborted TX\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
         GROUP BY application_name
         ORDER BY count(*) DESC;" 2>/dev/null

    echo ""
}

# Monitor test processes and their database usage
show_test_processes() {
    log_header "=== TEST PROCESS MONITORING ==="
    echo ""

    echo "Rust test processes currently running:"
    local rust_processes=$(pgrep -f "cargo.*test\|nextest\|test.*kapsel" 2>/dev/null || true)

    if [[ -n "$rust_processes" ]]; then
        echo "Found test processes:"
        for pid in $rust_processes; do
            local cmd=$(ps -p $pid -o pid,ppid,time,command --no-headers 2>/dev/null || echo "Process exited")
            echo "  $cmd"
        done
    else
        echo "No active test processes found"
    fi

    echo ""

    echo "Database connections from test processes:"
    # This shows connections that look like they're from test binaries
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            application_name as \"Application\",
            count(*) as \"Connections\",
            string_agg(DISTINCT state, ', ') as \"States\",
            min(backend_start) as \"Oldest Connection\",
            max(backend_start) as \"Newest Connection\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
           AND (application_name LIKE '%test%'
                OR application_name = ''
                OR application_name IS NULL)
         GROUP BY datname, application_name
         ORDER BY count(*) DESC;" 2>/dev/null

    echo ""
}

# Monitor connection churn and retry patterns
monitor_connection_churn() {
    log_header "=== CONNECTION CHURN MONITORING ==="
    echo ""

    echo "Starting 30-second connection monitoring (shows connection pattern changes)..."
    echo "Timestamp | Total Conns | Active | Idle | Idle in TX | Applications"
    echo "----------|-------------|--------|------|------------|-------------"

    local prev_total=0
    local start_time=$(date +%s)
    local end_time=$((start_time + 30))

    while [[ $(date +%s) -lt $end_time ]]; do
        local timestamp=$(date '+%H:%M:%S')

        local stats=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
            "SELECT
                count(*) as total,
                count(*) FILTER (WHERE state = 'active') as active,
                count(*) FILTER (WHERE state = 'idle') as idle,
                count(*) FILTER (WHERE state = 'idle in transaction') as idle_tx,
                count(DISTINCT application_name) as apps
             FROM pg_stat_activity
             WHERE datname IS NOT NULL;" 2>/dev/null | tr -d '\n' | sed 's/|/ /g')

        if [[ -n "$stats" ]]; then
            read -r total active idle idle_tx apps <<< "$stats"
            local change=""
            if [[ $total -ne $prev_total ]]; then
                local diff=$((total - prev_total))
                if [[ $diff -gt 0 ]]; then
                    change=" (+$diff)"
                else
                    change=" ($diff)"
                fi
            fi

            printf "%-9s | %-11s | %-6s | %-4s | %-10s | %-5s%s\n" \
                "$timestamp" "$total" "$active" "$idle" "$idle_tx" "$apps" "$change"

            prev_total=$total
        fi

        sleep 2
    done

    echo ""
    echo "Monitoring complete. Look for:"
    echo "- Sudden spikes in connection count (indicates connection leaks)"
    echo "- High 'Idle in TX' counts (indicates transaction leaks)"
    echo "- Frequent +/- changes (indicates connection churn/retry loops)"
    echo ""
}

# Debug connection acquisition timeouts
debug_connection_timeouts() {
    log_header "=== CONNECTION TIMEOUT DEBUGGING ==="
    echo ""

    echo "Current connection limits and usage:"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            'max_connections' as setting,
            setting::int as value,
            'Total connection limit' as description
         FROM pg_settings WHERE name = 'max_connections'
         UNION ALL
         SELECT
            'current_connections',
            count(*)::int,
            'Currently active connections'
         FROM pg_stat_activity WHERE datname IS NOT NULL
         UNION ALL
         SELECT
            'available_connections',
            (SELECT setting::int FROM pg_settings WHERE name = 'max_connections') - count(*)::int,
            'Available connection slots'
         FROM pg_stat_activity WHERE datname IS NOT NULL;" 2>/dev/null

    echo ""

    echo "Connection wait states (potential bottlenecks):"
    docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -c \
        "SELECT
            datname as \"Database\",
            state,
            wait_event_type,
            wait_event,
            count(*) as \"Count\",
            avg(EXTRACT(EPOCH FROM (now() - state_change)))::int as \"Avg Duration (s)\"
         FROM pg_stat_activity
         WHERE datname IS NOT NULL
           AND (wait_event_type IS NOT NULL OR state != 'active')
         GROUP BY datname, state, wait_event_type, wait_event
         ORDER BY count(*) DESC;" 2>/dev/null

    echo ""

    echo "Recommendations:"
    local total=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SELECT count(*) FROM pg_stat_activity WHERE datname IS NOT NULL;" 2>/dev/null | tr -d ' ')
    local max=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
        "SHOW max_connections;" 2>/dev/null | tr -d ' ')
    local usage_pct=$((total * 100 / max))

    if [[ $usage_pct -gt 80 ]]; then
        echo "     HIGH: Connection usage at ${usage_pct}% - likely cause of timeouts"
        echo "     - Reduce test parallelism or pool sizes"
        echo "     - Check for connection leaks in test cleanup"
    elif [[ $usage_pct -gt 60 ]]; then
        echo "     MEDIUM: Connection usage at ${usage_pct}% - monitor for spikes"
        echo "     - Consider reducing max_connections per pool"
    else
        echo "     LOW: Connection usage at ${usage_pct}% - healthy"
    fi
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

# Watch connection pools during test runs
watch_connection_pools() {
    log_info "Starting connection pool monitoring for test runs (Ctrl+C to exit)..."
    echo ""
    echo "This monitor shows real-time connection patterns during test execution."
    echo "Start your tests in another terminal to see the patterns."
    echo ""
    echo "Timestamp | DBs | Total | Active | Idle | TX | Apps | Test Procs | Pattern"
    echo "----------|-----|-------|--------|------|----|----- |-----------|--------"

    local prev_total=0
    local pattern_buffer=""
    local buffer_size=10

    while true; do
        local timestamp=$(date '+%H:%M:%S')

        # Get database and connection stats
        local db_count=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
            "SELECT count(*) FROM pg_database WHERE datname LIKE 'test_%';" 2>/dev/null | tr -d ' ')

        local conn_stats=$(docker exec "${CONTAINER_NAME}" psql -U "${POSTGRES_USER}" -d postgres -t -c \
            "SELECT
                count(*) as total,
                count(*) FILTER (WHERE state = 'active') as active,
                count(*) FILTER (WHERE state = 'idle') as idle,
                count(*) FILTER (WHERE state = 'idle in transaction') as idle_tx,
                count(DISTINCT application_name) as apps
             FROM pg_stat_activity
             WHERE datname IS NOT NULL;" 2>/dev/null | tr -d '\n' | sed 's/|/ /g')

        # Count test processes
        local test_proc_count=$(pgrep -f "cargo.*test\|nextest\|test.*kapsel" 2>/dev/null | wc -l)

        if [[ -n "$conn_stats" ]]; then
            read -r total active idle idle_tx apps <<< "$conn_stats"

            # Determine pattern
            local change=$((total - prev_total))
            local pattern_char="="
            if [[ $change -gt 5 ]]; then
                pattern_char="↑"  # Sudden increase
            elif [[ $change -lt -5 ]]; then
                pattern_char="↓"  # Sudden decrease
            elif [[ $change -gt 0 ]]; then
                pattern_char="+"  # Gradual increase
            elif [[ $change -lt 0 ]]; then
                pattern_char="-"  # Gradual decrease
            fi

            # Add to pattern buffer
            pattern_buffer="${pattern_buffer}${pattern_char}"
            if [[ ${#pattern_buffer} -gt $buffer_size ]]; then
                pattern_buffer="${pattern_buffer: -$buffer_size}"
            fi

            # Color code based on connection usage
            local color=""
            local usage_pct=$((total * 100 / 100))  # Assuming max 100 connections
            if [[ $usage_pct -gt 80 ]]; then
                color="${RED}"
            elif [[ $usage_pct -gt 60 ]]; then
                color="${YELLOW}"
            else
                color="${GREEN}"
            fi

            printf "${color}%-9s${NC} | %-3s | %-5s | %-6s | %-4s | %-2s | %-4s | %-9s | %-10s\n" \
                "$timestamp" "$db_count" "$total" "$active" "$idle" "$idle_tx" "$apps" "$test_proc_count" "$pattern_buffer"

            prev_total=$total
        fi

        sleep 1
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
  conn-details    Show detailed connection pool analysis
  test-procs      Show test process monitoring
  conn-churn      Monitor connection churn for 30s
  conn-debug      Debug connection acquisition timeouts
  resources, r    Show resource usage
  activity, a     Show database activity and locks
  cleanup         Clean up orphaned test databases
  watch, w        Continuous monitoring mode
  watch-pools     Watch connection pools during test runs
  full, f         Full detailed inspection
  help, h         Show this help

Examples:
  $0               # Show overview
  $0 watch         # Continuous monitoring
  $0 watch-pools   # Monitor connections during test runs
  $0 conn-debug    # Debug connection timeouts
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
        conn-details)
            show_connection_details
            ;;
        test-procs)
            show_test_processes
            ;;
        conn-churn)
            monitor_connection_churn
            ;;
        conn-debug)
            debug_connection_timeouts
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
        watch-pools)
            watch_connection_pools
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
