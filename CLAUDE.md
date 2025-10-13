# CLAUDE.md - Kapsel Development Context

**Purpose**: This document provides complete context for LLM-assisted development on Kapsel. It's both comprehensive and a gateway to deeper documentation. Read this first, validate assumptions against code, follow links for depth.

**Last Updated**: 2025-10-13 (commit 57de6a4)

---

## Quick Start

**What is Kapsel?** A webhook reliability service guaranteeing at-least-once delivery with cryptographically verifiable audit trails. We're the intermediary that ensures webhooks never get lost.

**Tech Stack**: Rust + Axum + PostgreSQL + TigerBeetle + proptest

**First Steps**:
1. Read this entire document (10 min)
2. Validate key assumptions (see [Self-Validation](#self-validation))
3. Follow links for subsystem-specific details
4. Check consistency before committing (see [Pre-Commit Checklist](#pre-commit-checklist))

---

## System Architecture

### Core Philosophy

Kapsel is built on three principles:

1. **Correctness by Construction** - Illegal states are unrepresentable
2. **Performance Without Compromise** - Zero-copy, lock-free async
3. **Observable by Default** - Every event has a correlation ID

### Architecture Patterns

- **PostgreSQL-only** - We removed SQLite for simplicity (commit 0f0c837)
- **Two-phase persistence** - PostgreSQL for operational durability, TigerBeetle for audit integrity
- **Deterministic testing** - TestClock gives us time control in tests
- **Invariant-driven** - Tests verify guarantees, not implementation

**Deep Dive**: See [docs/OVERVIEW.md](docs/OVERVIEW.md) for architecture diagrams and data flow.

---

## Development Workflow

### Build System

We use **both** `cargo` and `cargo make`:

| Command | When to Use |
|---------|-------------|
| `cargo test` | Standard test runner (works in CI) |
| `cargo make test` | Uses nextest (faster, better output) |
| `cargo make tdd` | Watch mode for TDD workflow |
| `cargo make check` | Full quality checks (format + lint + test + audit) |

**Key Point**: `cargo make test` is the default. CI uses `cargo nextest run --workspace`.

**Deep Dive**: See [Makefile.toml](Makefile.toml) for all available tasks.

### Git Workflow

**Conventional Commits** are mandatory. Format: `<type>(<scope>): <description>`

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `perf`, `chore`, `ci`

**Example Commits**:
```
feat(api): implement minimal webhook ingestion endpoint
fix(tests): add payload_size column to all INSERT queries
docs(testing): update testing strategy with property tests
style: fix clippy warnings
```

**Pre-commit hooks** run automatically (installed via cargo-husky):
1. `cargo fmt --all --check` - Formatting
2. `cargo clippy --all-targets --all-features -- -D warnings` - Linting
3. `cargo test --lib --workspace` - Fast unit tests

**Important**: Hooks only install during `cargo build`. If missing, manually copy from `.cargo-husky/hooks/` to `.git/hooks/` and `chmod +x`.

---

## Database Schema

### Source of Truth

**Primary**: `migrations/001_initial_schema.sql` (lines 1-311)
**Test Mirror**: `crates/test-harness/src/database.rs` (lines 92-198)

**Critical Invariant**: Test harness schema MUST match migrations. Schema divergence causes CI failures.

### Key Tables

```
tenants          - Multi-tenancy (id, name, plan, api_key)
endpoints        - Destination config (id, tenant_id, url, max_retries, circuit_state)
webhook_events   - Core event storage (id, tenant_id, endpoint_id, source_event_id,
                   status, payload_size, body, headers, received_at)
delivery_attempts - Audit trail (id, event_id, attempt_number, response_status,
                   attempted_at, duration_ms)
```

### Critical Constraints

**`payload_size` column** (added: commit 4c6b805):
- Type: `INTEGER NOT NULL`
- Constraint: `CHECK (payload_size > 0 AND payload_size <= 10485760)` (10MB max)
- **Gotcha**: Empty bodies need `payload_size = 1` minimum (not 0!)

**Validation**:
```rust
// WRONG - violates CHECK constraint
let payload_size = body.len() as i32; // Can be 0!

// CORRECT - enforces minimum
let payload_size = (body.len() as i32).max(1);
```

**Unique constraints**:
- `webhook_events`: `(tenant_id, endpoint_id, source_event_id)` - Idempotency
- `endpoints`: `(tenant_id, name)` - Named endpoints per tenant

### Schema Validation

**Check for drift**:
```bash
# Compare migration and test harness
diff <(grep "CREATE TABLE webhook_events" migrations/001_initial_schema.sql -A 20) \
     <(grep "CREATE TABLE.*webhook_events" crates/test-harness/src/database.rs -A 20)
```

**Expected**: Should only differ in syntax (SQL vs Rust), not structure.

---

## Testing Philosophy

### The Testing Contract

**"The test suite IS the system specification."**

Every test represents an invariant that must hold true. We don't test to find bugs—we test to prove correctness.

### Test Pyramid (Inverted)

```
Unit Tests       29% - Pure logic, no I/O
Property Tests   25% - Invariant validation with generated inputs
Integration      25% - Component boundaries with real database
Scenario Tests   15% - Multi-step workflows with time control
End-to-End        5% - Full system with HTTP server
Chaos Tests       1% - Production-like failure injection
```

**Why inverted?** Webhook reliability is about complex interactions, not isolated functions.

**Deep Dive**: See [docs/TESTING_STRATEGY.md](docs/TESTING_STRATEGY.md) for complete test patterns.

### Test Infrastructure

**TestEnv** (`crates/test-harness/src/lib.rs`):
```rust
let env = TestEnv::new().await?;  // Fresh PostgreSQL + HTTP mock + TestClock
env.clock.advance(Duration::from_secs(1));  // Deterministic time control
env.http_mock.mock_sequence()  // Chain HTTP responses
    .respond_with(503, "Unavailable")
    .respond_with(200, "OK")
    .build().await;
```

**Key Feature**: `TestClock` gives deterministic time control. No `tokio::time::sleep()` in tests!

### Property Testing with proptest

**Configuration** (via environment):
- `PROPTEST_CASES`: Number of test cases (default: 20 dev, 100 CI)
- `CI=true`: Enables CI-specific behavior

**Example**:
```rust
proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn retry_count_is_bounded(failures in 0u32..100, max_retries in 1u32..20) {
        let attempts = simulate_retries(failures, max_retries);
        prop_assert!(attempts <= max_retries + 1);  // +1 for initial attempt
    }
}
```

**Common Patterns**:
- Use `strategies::webhook_event_strategy()` for domain types
- Empty collections are valid test cases (check bounds!)
- CI runs more cases - catch edge cases early

**Deep Dive**: See [tests/property_tests.rs](tests/property_tests.rs) for strategy examples.

### Golden Sample Test

**Location**: `tests/golden_sample_test.rs::golden_webhook_delivery_with_retry_backoff`

This test embodies everything:
- Deterministic time control (exact backoff timing)
- Idempotency verification (duplicate detection)
- Circuit breaker behavior (fail-fast after threshold)
- Complete audit trail (all attempts recorded)
- Invariant validation (delivery guarantees)

**If this test passes, the core system works.**

---

## Code Style

### TIGERSTYLE Naming

**Functions**: `verb_object` pattern
```rust
deliver_webhook()     // NOT: deliverWebhook(), webhook_deliver()
validate_signature()  // NOT: signatureValidate(), validate_sign()
insert_event()        // NOT: insertNewEvent(), event_insert()
```

**Predicates**: `is_`, `has_`, `can_`
```rust
is_valid()      // NOT: valid(), check_valid()
has_expired()   // NOT: expired(), check_expired()
can_retry()     // NOT: retryable(), is_retryable()
```

**NO get_/set_ prefixes**:
```rust
// BAD
fn get_status(&self) -> Status
fn set_status(&mut self, status: Status)

// GOOD
fn status(&self) -> Status
fn update_status(&mut self, status: Status)
```

### Error Handling

**Library code** (crates/kapsel-core): Use `thiserror`
```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("webhook {id} not found")]
    NotFound { id: EventId },
}
```

**Application code**: Use `anyhow::Result`
```rust
let webhook = fetch_webhook(id)
    .await
    .context("failed to fetch webhook from database")?;
```

**Production code**: NEVER `unwrap()`, `expect()`, `panic!()`, or index without bounds check.

**Test code**: `unwrap()` and `expect()` are acceptable for clarity.

### Documentation

Document **WHY**, not **WHAT**:

```rust
// BAD: States the obvious
// Increment the counter
counter += 1;

// GOOD: Explains the decision
// Account for zero-indexing in user display
counter += 1;

// EXCELLENT: Explains non-obvious optimization
// PostgreSQL's SKIP LOCKED allows multiple workers to claim
// rows without blocking each other, giving us free work distribution
let query = sqlx::query!("SELECT * FROM events ... FOR UPDATE SKIP LOCKED");
```

**Deep Dive**: See [docs/STYLE.md](docs/STYLE.md) for complete style guide.

---

## Common Pitfalls & Solutions

### 1. Schema Drift (Test Harness vs Migrations)

**Symptom**: CI fails with "column does not exist" or "constraint violation"

**Root Cause**: Test harness schema (`crates/test-harness/src/database.rs`) doesn't match migrations.

**Solution**: Update both simultaneously:
1. Modify `migrations/001_initial_schema.sql`
2. Update `crates/test-harness/src/database.rs` CREATE TABLE
3. Add ALTER TABLE migration if column is new

**Prevention**: Run `cargo make test` locally before pushing.

**Recent Example**: Commit 4c6b805 - Added `payload_size` column

### 2. Empty Payloads Violate CHECK Constraint

**Symptom**: `new row violates check constraint "webhook_events_payload_size_check"`

**Root Cause**: `payload_size` must be > 0, but empty body gives 0.

**Solution**:
```rust
// WRONG
let payload_size = body.len() as i32;

// CORRECT
let payload_size = (body.len() as i32).max(1);
```

**Prevention**: Property tests will generate empty inputs - handle them!

**Recent Example**: Commit 57de6a4 - Fixed proptest failure with empty webhooks

### 3. Pre-commit Hooks Not Running

**Symptom**: Committing without format/lint checks

**Root Cause**: cargo-husky only installs during `cargo build`

**Solution**:
```bash
cp .cargo-husky/hooks/pre-commit .git/hooks/pre-commit
cp .cargo-husky/hooks/commit-msg .git/hooks/commit-msg
chmod +x .git/hooks/pre-commit .git/hooks/commit-msg
```

**Prevention**: Run `cargo build` at least once after cloning.

### 4. Test Flakiness from Non-Deterministic Time

**Symptom**: Tests pass locally, fail in CI randomly

**Root Cause**: Using `tokio::time::sleep()` or `std::time::Instant::now()`

**Solution**: Use `TestEnv.clock` for all time operations in tests:
```rust
// WRONG - non-deterministic
tokio::time::sleep(Duration::from_secs(1)).await;
let now = Instant::now();

// CORRECT - deterministic
env.clock.advance(Duration::from_secs(1));
let now = env.clock.now();
```

**Prevention**: Search for `tokio::time` in test files - should be zero occurrences.

### 5. Forgetting ON CONFLICT for Idempotency

**Symptom**: Unique constraint violation on duplicate webhook

**Root Cause**: Idempotency requires `ON CONFLICT` handling

**Solution**:
```rust
sqlx::query(
    "INSERT INTO webhook_events (...) VALUES (...)
     ON CONFLICT (tenant_id, endpoint_id, source_event_id) DO NOTHING"
)
```

**Prevention**: All webhook inserts must handle idempotency key conflicts.

---

## Pre-Commit Checklist

Before committing, verify:

### Schema Changes
- [ ] If `migrations/*.sql` changed → Update `crates/test-harness/src/database.rs`
- [ ] If adding NOT NULL column → Provide default or ensure all INSERTs include it
- [ ] If adding CHECK constraint → Verify all existing code satisfies it

### Database Operations
- [ ] All `INSERT INTO webhook_events` include `payload_size` column
- [ ] `payload_size` calculation uses `.max(1)` to handle empty payloads
- [ ] Idempotency keys use `ON CONFLICT ... DO NOTHING`

### Testing
- [ ] New features have property tests for edge cases
- [ ] Integration tests use `TestEnv.clock` for time control
- [ ] Golden sample test still passes (comprehensive invariant check)
- [ ] Run `cargo make test` locally (not just `cargo test`)

### Code Quality
- [ ] `cargo fmt` applied
- [ ] `cargo clippy` passes with zero warnings
- [ ] No `unwrap()` in production code paths
- [ ] Error context added with `.context("descriptive message")`

### Documentation
- [ ] Complex decisions documented with WHY
- [ ] Public APIs have doc comments
- [ ] If architectural change → Update `docs/ARCHITECTURE.md`
- [ ] If new test pattern → Update `docs/TESTING_STRATEGY.md`

---

## Self-Validation

When assumptions seem wrong, verify against code:

### Database Schema Sync Check

```bash
# 1. Check webhook_events has payload_size in migration
grep -A 20 "CREATE TABLE webhook_events" migrations/001_initial_schema.sql | grep payload_size

# Expected: Should find "payload_size INTEGER NOT NULL"
# If not found: Schema drift! Update migration.

# 2. Check test harness matches
grep -A 20 "CREATE TABLE.*webhook_events" crates/test-harness/src/database.rs | grep payload_size

# Expected: Should find "payload_size INTEGER NOT NULL"
# If not found: Update test harness schema.

# 3. Verify CHECK constraint
grep "CHECK.*payload_size" migrations/001_initial_schema.sql

# Expected: CHECK (payload_size > 0 AND payload_size <= 10485760)
```

### Test Configuration Check

```bash
# 1. Verify property test config
grep "proptest_config" tests/property_tests.rs

# Expected: Should use centralized config function

# 2. Check PROPTEST_CASES handling
grep "PROPTEST_CASES" tests/property_tests.rs

# Expected: Falls back to 20 (dev) or 100 (CI) if not set
```

### Hook Installation Check

```bash
# Check if hooks are installed
ls -la .git/hooks/pre-commit .git/hooks/commit-msg

# Expected: Both files exist and are executable
# If missing: Run installation commands from Pitfall #3
```

### Quick Test Run

```bash
# Verify entire test suite passes
cargo make test

# Expected: "55 tests run: 55 passed, 0 skipped"
# If failures: Check recent commits for schema changes
```

---

## Documentation Map

```
CLAUDE.md (you are here)          → System-wide context for LLM sessions
├── README.md                     → Human-friendly project introduction
├── docs/OVERVIEW.md              → Architecture, data flow, tech stack
├── docs/SPECIFICATION.md         → Detailed requirements and API contracts
├── docs/TESTING_STRATEGY.md      → Complete testing philosophy and patterns
├── docs/TESTING.md               → Quick testing reference
├── docs/STYLE.md                 → Code style guide (TIGERSTYLE naming)
├── docs/IMPLEMENTATION_STATUS.md → What's built, what's planned
├── Makefile.toml                 → All cargo make tasks explained
├── migrations/                   → Database schema source of truth
├── crates/test-harness/          → Test infrastructure API
│   └── README.md                 → Test harness usage guide
└── tests/                        → Integration and property tests
    ├── property_tests.rs         → Invariant validation
    ├── golden_sample_test.rs     → Golden path verification
    └── webhook_ingestion_test.rs → HTTP ingestion flow
```

**Navigation Rule**: Start here, follow links for depth, return for context.

---

## Debugging Strategy

### Test Failures

1. **Read the error message completely** - It usually tells you exactly what's wrong
2. **Check recent commits** - Schema changes? New constraints?
3. **Validate schema sync** - Run self-validation checks above
4. **Run test in isolation** - `cargo test --test <test_name> <specific_test>`
5. **Add debug output** - Use `dbg!()` or `eprintln!()` liberally

### CI Failures

1. **Compare local vs CI environment**:
   - `PROPTEST_CASES`: 20 local, 100 CI (more edge cases)
   - Database: Docker locally, GitHub Services in CI

2. **Check for race conditions** - Use `TestClock`, not real time
3. **Verify hooks ran** - Local commit should pass same checks as CI

### Performance Issues

1. **Run benchmarks** - `cargo make bench`
2. **Check KPIs** (see `benches/webhook_benchmarks.rs`):
   - Ingestion latency p99 < 50ms
   - Delivery throughput > 1000 webhooks/sec
   - Database query p99 < 5ms
   - Memory per 1K webhooks < 200MB

3. **Profile if needed** - `cargo make flamegraph`

---

## Quick Reference

### Most Common Commands

```bash
# Development
cargo make tdd                    # Watch mode for TDD
cargo make dev                    # Run server with auto-reload

# Testing
cargo make test                   # Run all tests (use this!)
cargo test --test property_tests  # Run specific test file
cargo make test-coverage          # Generate coverage report

# Quality
cargo make check                  # Format + lint + test + audit
cargo make format                 # Auto-format code
cargo make lint                   # Run clippy

# Database
cargo make db-reset              # Fresh database with migrations
cargo make db-migrate            # Apply pending migrations
```

### Key Files to Know

- `Cargo.toml` (root) - Workspace config, feature flags, dependencies
- `migrations/001_initial_schema.sql` - Schema source of truth
- `crates/test-harness/src/database.rs` - Test schema (must match migrations!)
- `tests/golden_sample_test.rs` - The test that proves everything works
- `.cargo-husky/hooks/` - Pre-commit hook definitions

### Environment Variables

```bash
DATABASE_URL          # PostgreSQL connection (default: localhost:5433)
PROPTEST_CASES        # Property test iterations (default: 20 dev, 100 CI)
CI=true               # Enables CI-specific behavior
RUST_BACKTRACE=1      # Full backtraces on panic
RUST_LOG=debug        # Logging level
```

---

## Consistency Invariants

These must ALWAYS be true:

1. **Schema Sync**: Test harness schema matches migrations exactly
2. **Idempotency Keys**: All webhook inserts handle `ON CONFLICT`
3. **Payload Size**: Never 0, always between 1 and 10MB
4. **Time Control**: Tests use `TestClock`, never `tokio::time`
5. **No Panics**: Production code never uses `unwrap()` or `panic!()`
6. **Conventional Commits**: Every commit message follows format
7. **Zero Clippy Warnings**: Code passes `clippy -- -D warnings`
8. **Test Coverage**: New features have property + integration tests

**If any invariant is violated, fix immediately before proceeding.**

---

## When CLAUDE.md Seems Wrong

This document is validated against commit **57de6a4** (2025-10-13).

If something seems outdated:

1. **Check the code** - Trust the implementation over this doc
2. **Run validation checks** - See [Self-Validation](#self-validation)
3. **Look for recent commits** - `git log --oneline -20`
4. **Check related docs** - They may have newer info
5. **Update this file** - Then commit with: `docs(claude): update CLAUDE.md for <change>`

**Recent significant changes**:
- 57de6a4: Fix payload_size constraint (min value 1)
- 4c6b805: Add payload_size column to schema
- 096e8fb: Fix clippy warnings after test infrastructure
- 0f0c837: Remove SQLite, PostgreSQL-only

---

## Final Word

**This document exists to make every LLM session productive from minute one.**

When in doubt:
1. Read the code (source of truth)
2. Check this document (context and patterns)
3. Follow links (deeper understanding)
4. Validate assumptions (trust but verify)
5. Update docs (keep knowledge current)

**The test suite is the specification. If tests pass, the system works.**

Good luck building Kapsel. Write boring code that works reliably.
