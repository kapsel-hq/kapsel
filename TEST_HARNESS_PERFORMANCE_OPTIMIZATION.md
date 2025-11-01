# Test Harness Performance Optimization Plan

## Executive Summary

The Kapsel test suite has regressed from 20 seconds to **98.9 seconds** after migrating from isolated databases to a shared database with transaction-based isolation. The root cause is **NOT** the shared database strategy itself, but implementation bottlenecks that serialize test startup across 39 parallel processes.

**Key Insight:** Nextest runs ~39 test binaries in parallel as separate processes. Each process independently tries to:

1. Acquire a PostgreSQL advisory lock
2. Run migrations and create template database
3. Clean up orphaned databases
4. Initialize its own connection pool

This causes **38 processes to block** waiting for 1 process to complete initialization.

**Target:** Reduce test execution from 98.9s to <20s (80% improvement)

## Current Architecture

### Test Distribution

- **343 total tests** across 36 binaries
- **~75% use shared database** (`TestEnv::new_shared()`) with transaction rollback
- **~25% use isolated databases** (`TestEnv::new_isolated()`) for delivery engine tests
- **8 CPU cores** with nextest default parallelism
- **39 processes** competing for resources simultaneously

### Bottleneck Analysis

#### 1. PostgreSQL Advisory Lock Forces Serial Initialization ⚠️ **CRITICAL**

Every test process calls `ensure_template_and_cleanup()`:

```rust
// 39 processes all hit this:
let acquired_lock: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
    .bind(CLEANUP_LOCK_KEY)
    .fetch_one(&admin_pool)
    .await?;
```

**Impact:** First process wins, 38 others wait. Serial initialization takes ~2-3s per process.

#### 2. Connection Pool Starvation ⚠️ **HIGH**

Current configuration:

- Shared pool: **15 max connections**
- 39 processes × multiple tests each = **hundreds of connection requests**
- PostgreSQL default max_connections: 100

**Result:** Connection exhaustion and timeouts.

#### 3. Missing Cleanup for Isolated Databases ⚠️ **MEDIUM**

`IsolatedTestDatabase` lacks a `Drop` implementation:

```rust
pub struct IsolatedTestDatabase {
    pool: PgPool,
    database_name: String,
}
// No Drop impl - databases orphaned!
```

#### 4. Redundant Migration Execution ⚠️ **MEDIUM**

Every process runs migrations independently:

```rust
sqlx::migrate::Migrator::new(migrations_dir)
    .await?
    .run(&main_pool)
    .await?;
```

## Optimization Strategy

### Phase 1: Eliminate Advisory Lock Bottleneck (Target: -60s)

The PostgreSQL advisory lock is the primary bottleneck. We need a solution that works across processes.

#### Solution A: Remove Lock Entirely (Preferred)

**Insight:** If the database is already set up correctly, initialization is idempotent.

```rust
async fn ensure_template_database_fast(admin_pool: &PgPool) -> Result<()> {
    // Check if template exists (fast query)
    let template_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(TEMPLATE_DB_NAME)
            .fetch_one(admin_pool)
            .await?;

    if template_exists {
        // Fast path: 99% of tests take this
        return Ok(());
    }

    // Slow path: Only first process does this
    // Use CREATE DATABASE IF NOT EXISTS pattern
    match sqlx::query(&format!(
        "CREATE DATABASE \"{}\" WITH TEMPLATE \"{}\"",
        TEMPLATE_DB_NAME, MAIN_TEST_DB_NAME
    ))
    .execute(admin_pool)
    .await
    {
        Ok(_) => info!("Created template database"),
        Err(e) if is_already_exists_error(&e) => {
            // Another process created it, that's fine
            debug!("Template database already exists");
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

// Cleanup runs asynchronously, doesn't block test startup
async fn async_cleanup_old_databases(admin_pool: PgPool) {
    tokio::spawn(async move {
        if let Err(e) = cleanup_old_test_databases(&admin_pool).await {
            warn!("Background cleanup failed: {}", e);
        }
    });
}
```

#### Solution B: File-Based Coordination

Use filesystem for cross-process synchronization:

```rust
use std::fs;
use std::path::Path;

static INIT_MARKER: &str = "/tmp/kapsel_test_initialized";

async fn ensure_template_database_with_file_lock() -> Result<()> {
    // Fast path: Check marker file
    if Path::new(INIT_MARKER).exists() {
        return Ok(());
    }

    // Slow path: First process creates marker
    let admin_pool = create_admin_pool().await?;

    // Use filesystem atomic operations
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)  // Atomic - fails if exists
        .open(INIT_MARKER)
    {
        Ok(_) => {
            // We won, do initialization
            ensure_template_database(&admin_pool).await?;
            cleanup_old_test_databases(&admin_pool).await?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another process is initializing, wait
            for _ in 0..100 {
                if Path::new(INIT_MARKER).exists() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}
```

### Phase 2: Fix Connection Pool Sizing (Target: -20s)

#### Problem: Global Connection Exhaustion

39 processes × 15 connections = 585 connections needed, but PostgreSQL defaults to 100.

#### Solution: Dynamic Pool Sizing

```rust
async fn create_shared_pool() -> Result<PgPool> {
    // Detect parallelism level
    let max_processes = 40;  // Conservative estimate for nextest
    let connections_per_process = 2;  // Minimum viable

    let opts = database_url
        .parse::<PgConnectOptions>()?
        .database(MAIN_TEST_DB_NAME);

    let pool = PgPoolOptions::new()
        .max_connections(connections_per_process)
        .min_connections(1)
        .max_lifetime(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(2))  // Fast fail
        .connect_with(opts)
        .await?;

    Ok(pool)
}
```

Also update PostgreSQL configuration:

```yaml
# docker-compose.yml
postgres-test:
  command:
    - postgres
    - -c
    - max_connections=200 # Support parallel testing
```

### Phase 3: Implement Database Cleanup (Target: -5s)

#### Solution: Drop Implementation with Best-Effort Cleanup

```rust
impl Drop for IsolatedTestDatabase {
    fn drop(&mut self) {
        let database_name = self.database_name.clone();
        let database_url = std::env::var("DATABASE_URL").ok();

        // Best-effort cleanup - don't block test exit
        std::thread::spawn(move || {
            if let Some(url) = database_url {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .ok();

                if let Some(rt) = rt {
                    rt.block_on(async {
                        if let Ok(admin_pool) = create_admin_pool_from_url(&url).await {
                            let _ = drop_database_immediate(&admin_pool, &database_name).await;
                        }
                    });
                }
            }
        });
    }
}
```

### Phase 4: Optimize Database Configuration (Target: -10s)

#### PostgreSQL Tuning for Ephemeral Tests

```yaml
# docker-compose.yml
postgres-test:
  image: postgres:16-alpine
  tmpfs:
    - /var/lib/postgresql/data:size=2g,mode=1777
  environment:
    POSTGRES_USER: postgres
    POSTGRES_PASSWORD: postgres
    POSTGRES_DB: kapsel_test
  command:
    - postgres
    # Connection settings
    - -c
    - max_connections=200
    # Performance settings for tmpfs
    - -c
    - fsync=off
    - -c
    - synchronous_commit=off
    - -c
    - full_page_writes=off
    - -c
    - checkpoint_segments=32
    - -c
    - checkpoint_completion_target=0.9
    # Memory settings
    - -c
    - shared_buffers=256MB
    - -c
    - work_mem=16MB
    - -c
    - maintenance_work_mem=128MB
    # Planner settings for fast queries
    - -c
    - random_page_cost=1.0
    - -c
    - seq_page_cost=1.0
    - -c
    - cpu_tuple_cost=0.01
    # Disable logging for speed
    - -c
    - log_statement=none
    - -c
    - log_duration=off
```

### Phase 5: Migration Optimization (Target: -3s)

#### Solution: Check Before Running

```rust
async fn ensure_migrations_current(pool: &PgPool) -> Result<bool> {
    // Quick check if migrations needed
    let current_version: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM _sqlx_migrations ORDER BY version DESC LIMIT 1"
    )
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    let latest_version = get_latest_migration_version()?;

    if current_version == Some(latest_version) {
        return Ok(true);  // Already current
    }

    // Run migrations only if needed
    let migrations_dir = find_migrations_directory()?;
    sqlx::migrate::Migrator::new(migrations_dir)
        .await?
        .run(pool)
        .await?;

    Ok(false)
}
```

## Implementation Checklist

### Immediate Actions (Today)

- [ ] Remove advisory lock bottleneck
- [ ] Reduce connection pool to 2 connections per process
- [ ] Add Drop implementation for IsolatedTestDatabase
- [ ] Update docker-compose with optimized PostgreSQL settings

### Quick Wins (This Week)

- [ ] Implement file-based initialization marker
- [ ] Add connection pool monitoring/metrics
- [ ] Skip migration runs when already current
- [ ] Background cleanup for orphaned databases

### Future Improvements

- [ ] Consider connection pooling proxy (PgBouncer)
- [ ] Investigate test batching to reduce process count
- [ ] Profile and optimize slowest individual tests
- [ ] Add test performance regression detection in CI

## Profiling Commands

```bash
# Measure baseline
time cargo nextest run --workspace

# Profile with detailed timing
cargo nextest run --workspace --profile=ci-timing

# Find slowest tests
cargo nextest run --workspace --slow-timeout 1s

# Monitor PostgreSQL connections
watch -n 1 'psql -U postgres -h localhost -p 5433 -c "SELECT count(*) FROM pg_stat_activity"'

# Check for orphaned databases
psql -U postgres -h localhost -p 5433 -c "SELECT datname FROM pg_database WHERE datname LIKE 'test_%'"
```

## Success Metrics

- **Primary:** Test suite completes in <20s (from 98.9s)
- **Connection health:** No connection timeout errors
- **Resource cleanup:** Zero orphaned databases after test run
- **Reliability:** 100% pass rate maintained
- **Developer experience:** Tests provide rapid feedback

## Risk Mitigation

**Risk:** Removing locks causes race conditions
**Mitigation:** PostgreSQL's `CREATE DATABASE` is atomic; handle "already exists" errors gracefully

**Risk:** Reduced connection pools cause starvation
**Mitigation:** Fast acquire timeout with clear error messages

**Risk:** Cleanup failures leave orphaned databases
**Mitigation:** Periodic cleanup job, age-based deletion

## Conclusion

The 98.9s test runtime is caused by serialized initialization across 39 parallel processes, not the shared database strategy. By removing the advisory lock bottleneck and right-sizing connection pools for parallel execution, we can achieve <20s test runs while maintaining reliability and determinism.

The key insight: **optimize for parallel process execution**, not just parallel test execution within a process.
