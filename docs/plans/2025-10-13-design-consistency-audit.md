# Design Consistency Audit Plan

**Created**: 2025-10-13
**Goal**: Comprehensive review of Kapsel's current implementation to identify and fix any inconsistencies with the design specification before building further features.
**Context**: Foundation is complete (ingestion endpoint + test harness). Before implementing delivery engine, ensure everything aligns with SPECIFICATION.md, OVERVIEW.md, and TESTING_STRATEGY.md.

---

## Overview

This audit systematically validates that implemented code matches documented design across:
- Database schema alignment
- API contracts and error handling
- Type safety and domain models
- Testing infrastructure completeness
- Code style consistency
- Documentation accuracy

Each task is independent and can be done in any order.

---

## Task 1: Database Schema Consistency Audit

**Time**: 45 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify database schema matches specification and test harness matches migrations.

### Acceptance Criteria

- [ ] Migrations match SPECIFICATION.md schema (lines 152-243)
- [ ] Test harness schema matches migrations exactly
- [ ] All constraints documented in SPECIFICATION are implemented
- [ ] No extra columns exist that aren't documented
- [ ] Indexes match performance requirements

### How to Verify

```bash
# 1. Compare migration to spec
diff <(grep -A 30 "CREATE TABLE webhook_events" migrations/001_initial_schema.sql) \
     <(grep -A 30 "CREATE TABLE webhook_events" docs/SPECIFICATION.md)

# 2. Compare test harness to migration
diff <(grep -A 20 "CREATE TABLE.*webhook_events" crates/test-harness/src/database.rs) \
     <(grep -A 30 "CREATE TABLE webhook_events" migrations/001_initial_schema.sql)

# 3. Check all tables exist
psql $DATABASE_URL -c "\dt" | grep -E "webhook_events|delivery_attempts|endpoints|tenants"

# 4. Verify constraints
psql $DATABASE_URL -c "\d+ webhook_events" | grep -E "CHECK|UNIQUE|NOT NULL"
```

### Known Issues to Check

- ✅ `payload_size` column exists (added commit 4c6b805)
- Check if `status` enum matches SPEC: 'received', 'pending', 'delivering', 'success', 'failed'
- Check if we're missing `delivered` vs `success` status (SPEC says 'success', code uses 'delivered')
- Verify `tigerbeetle_id` column exists in migration
- Confirm circuit breaker columns exist in endpoints table

### If Issues Found

1. **Schema mismatch**: Create migration for missing columns
2. **Test harness drift**: Update `crates/test-harness/src/database.rs`
3. **Documentation error**: Update SPECIFICATION.md to match reality
4. Decision: Which is source of truth? (Suggest: migrations are truth)

---

## Task 2: Domain Model vs Database Schema Alignment

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify Rust domain models in `kapsel-core` match database schema.

### Acceptance Criteria

- [ ] `WebhookEvent` struct fields match `webhook_events` columns
- [ ] `Endpoint` struct fields match `endpoints` columns
- [ ] `DeliveryAttempt` struct fields match `delivery_attempts` columns
- [ ] Enums match database TEXT constraints
- [ ] All NOT NULL columns are non-Option in Rust
- [ ] All nullable columns are Option<T> in Rust

### How to Verify

```bash
# 1. Compare WebhookEvent fields to webhook_events columns
grep -A 30 "pub struct WebhookEvent" crates/kapsel-core/src/models.rs
psql $DATABASE_URL -c "\d webhook_events"

# 2. Check status enum
grep -A 10 "pub enum EventStatus" crates/kapsel-core/src/models.rs
psql $DATABASE_URL -c "SELECT constraint_name, check_clause FROM information_schema.check_constraints WHERE table_name='webhook_events';"

# 3. Verify circuit state enum
grep -A 5 "pub enum CircuitState" crates/kapsel-core/src/models.rs
```

### Known Issues to Check

- Does `EventStatus` include all states from SPEC? ('received', 'pending', 'delivering', 'delivered', 'failed', 'dead_letter')
- SPEC says 'success' but code might use 'delivered' - which is correct?
- Are there fields in struct not in database? (technical debt)
- Are there columns in database not in struct? (missing modeling)

### If Issues Found

1. **Missing enum variant**: Add to domain model and migration
2. **Nullability mismatch**: Fix Option<T> vs T in struct
3. **Extra struct fields**: Remove or justify with comment
4. **Missing struct fields**: Add with proper types

---

## Task 3: Error Taxonomy Completeness Check

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify all error codes from SPECIFICATION.md (lines 342-369) are implemented in `kapsel-core/src/error.rs`.

### Acceptance Criteria

- [ ] All E1001-E1005 application errors exist
- [ ] All E2001-E2005 delivery errors exist
- [ ] All E3001-E3004 system errors exist
- [ ] Each error has correct retry logic (matches SPEC table)
- [ ] Error codes match exactly (no typos like E1001 vs E10001)
- [ ] HTTP status mapping is correct

### How to Verify

```bash
# 1. List all error variants
grep "pub enum.*Error" crates/kapsel-core/src/error.rs -A 50

# 2. Check error codes exist
for code in E1001 E1002 E1003 E1004 E1005 E2001 E2002 E2003 E2004 E2005 E3001 E3002 E3003 E3004; do
    grep -n "$code" crates/kapsel-core/src/error.rs || echo "MISSING: $code"
done

# 3. Verify retry logic
grep "is_retryable" crates/kapsel-core/src/error.rs -A 20
```

### Known Issues to Check

- Are all 14 error codes present?
- Does `is_retryable()` match SPEC retry column?
- Are error messages descriptive enough?
- Is there HTTP status mapping? (should be)

### If Issues Found

1. **Missing error code**: Add variant to enum
2. **Wrong retry logic**: Fix `is_retryable()` implementation
3. **Missing HTTP mapping**: Add method for status codes
4. **Unclear messages**: Improve error text

---

## Task 4: API Contract Validation

**Time**: 45 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify API endpoints match SPECIFICATION.md (lines 269-338).

### Acceptance Criteria

- [ ] POST /ingest/{endpoint_id} accepts correct headers
- [ ] Response codes match spec (200, 400, 413, 429, 503)
- [ ] Error responses include error codes
- [ ] Idempotency-Key header is processed
- [ ] Signature validation headers are checked (if configured)
- [ ] Response format matches spec (event_id returned)

### How to Verify

```bash
# 1. Check handler signature
grep "async fn ingest" crates/kapsel-api/src/handlers/ingest.rs -A 10

# 2. Check status code mapping
grep "StatusCode::" crates/kapsel-api/src/handlers/ingest.rs

# 3. Verify error response format
grep "struct.*Response" crates/kapsel-api/src/handlers/ingest.rs -A 5

# 4. Test against running server
cargo run &
SERVER_PID=$!
sleep 2

# Happy path
curl -X POST http://localhost:8080/ingest/test-endpoint \
  -H "Content-Type: application/json" \
  -d '{"test": "data"}' \
  -v

# Should return 200 with event_id

# Too large payload
dd if=/dev/zero bs=1M count=11 | curl -X POST http://localhost:8080/ingest/test-endpoint \
  -H "Content-Type: application/json" \
  --data-binary @- \
  -v

# Should return 413

kill $SERVER_PID
```

### Known Issues to Check

- Is payload size limit enforced (10MB)?
- Are all required headers extracted?
- Is idempotency working correctly?
- Are error responses structured (JSON with code)?

### If Issues Found

1. **Missing validation**: Add to handler
2. **Wrong status code**: Fix mapping
3. **Missing header**: Add extraction logic
4. **Poor error format**: Improve response struct

---

## Task 5: Test Infrastructure Completeness

**Time**: 45 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify test infrastructure matches TESTING_STRATEGY.md requirements.

### Acceptance Criteria

- [ ] TestEnv provides all required capabilities
- [ ] TestClock is used for deterministic time
- [ ] MockServer supports scenario building
- [ ] Fixture builders exist for common types
- [ ] Property test strategies exist for domain types
- [ ] Golden sample test covers core flow

### How to Verify

```bash
# 1. Check TestEnv capabilities
grep "pub struct TestEnv" crates/test-harness/src/lib.rs -A 20

# 2. Verify TestClock exists and is used
grep -r "TestClock" tests/

# 3. Check MockServer features
grep "impl MockServer" crates/test-harness/src/http.rs -A 50

# 4. Find fixture builders
grep "Builder" crates/test-harness/src/fixtures.rs

# 5. Verify property test strategies
grep "Strategy" crates/test-harness/src/invariants.rs -A 10

# 6. Run golden sample test
cargo test golden_webhook_delivery_with_retry_backoff
```

### Known Issues to Check

- Is TestClock actually used in tests? (no tokio::time::sleep)
- Do we have builders for WebhookEvent, Endpoint?
- Are property test strategies exported?
- Does golden test verify all invariants?

### If Issues Found

1. **Missing capability**: Add to TestEnv
2. **Non-deterministic time**: Replace with TestClock
3. **No builders**: Create fixture builders
4. **Weak golden test**: Add more assertions

---

## Task 6: Code Style Consistency Audit

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify code follows STYLE.md conventions (TIGERSTYLE).

### Acceptance Criteria

- [ ] Functions use `verb_object` naming
- [ ] No `get_`/`set_` prefixes anywhere
- [ ] Predicates use `is_`, `has_`, `can_`
- [ ] Error handling uses `anyhow::Context`
- [ ] No `unwrap()` in production code paths
- [ ] No `panic!()` anywhere except tests
- [ ] Comments explain WHY not WHAT

### How to Verify

```bash
# 1. Find get_/set_ violations
grep -r "fn get_\|fn set_" crates/ --include="*.rs" | grep -v test

# 2. Find unwrap in production code
grep -r "\.unwrap()" crates/kapsel-api/src crates/kapsel-core/src --include="*.rs"

# 3. Find panic in production code
grep -r "panic!" crates/kapsel-api/src crates/kapsel-core/src --include="*.rs"

# 4. Check predicate naming
grep -r "fn.*valid\|fn.*expired" crates/ --include="*.rs" | grep -v "is_\|has_"

# 5. Check error context usage
grep -r "\.context(" crates/kapsel-api/src --include="*.rs"
```

### Known Issues to Check

- Are there any get_status() instead of status()?
- Any unwrap() in API handlers?
- Missing .context() on error propagation?
- Any clever one-liners that should be expanded?

### If Issues Found

1. **Bad naming**: Rename functions (breaking change if public)
2. **Unwrap in prod**: Replace with proper error handling
3. **Missing context**: Add descriptive .context() calls
4. **Unclear code**: Add WHY comments or simplify

---

## Task 7: Documentation Accuracy Check

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify documentation matches implementation.

### Acceptance Criteria

- [ ] CLAUDE.md matches current commit (validated: 57de6a4)
- [ ] IMPLEMENTATION_STATUS.md reflects actual completion
- [ ] README.md example code works
- [ ] Module docs match public API
- [ ] All public functions have doc comments

### How to Verify

```bash
# 1. Check for undocumented public items
cargo doc 2>&1 | grep warning

# 2. Verify README example compiles
grep -A 20 "```rust" README.md > /tmp/readme_example.rs
# Manually check if this would compile

# 3. Check CLAUDE.md is current
head -1 CLAUDE.md | grep "57de6a4"

# 4. Validate IMPLEMENTATION_STATUS claims
grep "COMPLETED" docs/IMPLEMENTATION_STATUS.md -B 2

# 5. Run doc tests
cargo test --doc
```

### Known Issues to Check

- Are there public APIs without docs?
- Does README have outdated examples?
- Is IMPLEMENTATION_STATUS overly optimistic?
- Are there TODO comments in docs?

### If Issues Found

1. **Missing docs**: Add rustdoc comments
2. **Outdated examples**: Update README
3. **Wrong status**: Update IMPLEMENTATION_STATUS
4. **Stale CLAUDE.md**: Update validation date

---

## Task 8: Type Safety and Newtype Validation

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify we're using newtypes instead of primitives.

### Acceptance Criteria

- [ ] UUIDs are wrapped in EventId, TenantId, EndpointId
- [ ] No raw String for IDs in function signatures
- [ ] No primitive obsession (raw i32 for status codes)
- [ ] Timestamps use proper types (not i64)
- [ ] HTTP status codes use StatusCode, not u16

### How to Verify

```bash
# 1. Find raw UUID usage
grep -r "Uuid" crates/kapsel-api/src crates/kapsel-core/src --include="*.rs" | \
  grep -v "EventId\|TenantId\|EndpointId"

# 2. Find String ID parameters
grep -r "fn.*String" crates/kapsel-api/src --include="*.rs" | grep id

# 3. Check for primitive status codes
grep -r "u16\|i32" crates/kapsel-api/src --include="*.rs" | grep status

# 4. Verify StatusCode usage
grep "StatusCode::" crates/kapsel-api/src/handlers/ --include="*.rs"
```

### Known Issues to Check

- Are we passing raw Uuid instead of EventId?
- Any String IDs that should be typed?
- Direct integer status codes instead of enum?
- Missing newtype wrappers?

### If Issues Found

1. **Raw UUID**: Wrap in newtype
2. **String ID**: Create typed ID
3. **Primitive status**: Use proper enum
4. **Missing wrapper**: Add newtype with conversions

---

## Task 9: Consistency Between Tests and Production

**Time**: 45 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify tests exercise same code paths as production.

### Acceptance Criteria

- [ ] Test database schema matches production migrations
- [ ] Test fixtures create valid domain objects
- [ ] Integration tests use real HTTP server (not mocked handler)
- [ ] Property tests generate realistic data
- [ ] Test error cases match production error handling

### How to Verify

```bash
# 1. Compare test schema to migrations (done in Task 1, verify again)
diff crates/test-harness/src/database.rs migrations/001_initial_schema.sql

# 2. Check if tests use actual server
grep "TestEnv::with_server" tests/*.rs

# 3. Verify fixture validity
cargo test --test webhook_ingestion_test -- --nocapture

# 4. Check property test realism
grep "Strategy" tests/property_tests.rs -A 10

# 5. Run all tests and check for flakiness
for i in {1..5}; do cargo make test || echo "FLAKY: Run $i failed"; done
```

### Known Issues to Check

- Do test fixtures create invalid data?
- Are tests mocking too much? (should use real DB)
- Property tests generating unrealistic inputs?
- Schema drift between test and prod?

### If Issues Found

1. **Invalid fixtures**: Fix builder logic
2. **Over-mocking**: Use real components in tests
3. **Unrealistic property tests**: Improve strategies
4. **Schema drift**: Sync test harness (see Task 1)

---

## Task 10: Security and Safety Audit

**Time**: 30 minutes
**Status**: 🔴 Not Started

### What You're Checking

Verify security requirements from SPECIFICATION.md (lines 108-120) are met.

### Acceptance Criteria

- [ ] No unsafe code in application crates
- [ ] SQL injection not possible (using parameterized queries)
- [ ] Secrets not logged or exposed
- [ ] Input validation on all endpoints
- [ ] Rate limiting considered (even if not implemented)

### How to Verify

```bash
# 1. Find any unsafe blocks
grep -r "unsafe" crates/kapsel-api/src crates/kapsel-core/src --include="*.rs"

# 2. Check for SQL injection risks
grep -r "format!\|&format" crates/ --include="*.rs" | grep -i "select\|insert\|update"

# 3. Find potential secret exposure
grep -r "println!\|eprintln!" crates/kapsel-api/src --include="*.rs"
grep -r "secret\|password\|key" crates/ --include="*.rs" | grep -i "log\|print\|debug"

# 4. Verify input validation
grep "validate" crates/kapsel-api/src/handlers/ --include="*.rs"

# 5. Check for hardcoded secrets
grep -r "whsec_\|sk_\|pk_" crates/ --include="*.rs" | grep -v "example\|test"
```

### Known Issues to Check

- Any unsafe blocks? (should be none)
- String concatenation for SQL? (should use sqlx macros)
- Secrets in logs? (should use [REDACTED])
- Missing input validation? (size, format, etc.)

### If Issues Found

1. **Unsafe code**: Rewrite with safe Rust
2. **SQL injection risk**: Use parameterized queries
3. **Secret exposure**: Redact in logs
4. **Missing validation**: Add checks

---

## Summary and Next Steps

### After Completing All Tasks

1. **Compile findings** into a summary document
2. **Prioritize fixes**: Critical (security/data loss) → High (correctness) → Medium (consistency) → Low (style)
3. **Create fix plan**: Break fixes into commits following conventional commit format
4. **Update CLAUDE.md**: Document any discovered patterns or gotchas
5. **Update IMPLEMENTATION_STATUS**: Reflect true state

### Expected Outcome

- **Green**: Everything matches, proceed with delivery engine
- **Yellow**: Minor inconsistencies, fix before new features
- **Red**: Major issues, must fix immediately

### Metrics of Success

- [ ] All tasks completed with findings documented
- [ ] Schema drift eliminated (test harness = migrations)
- [ ] No missing error codes
- [ ] No unwrap() in production code
- [ ] Documentation matches reality
- [ ] Tests are deterministic and comprehensive

---

## Notes

- Each task is independent - start with any
- Use `git grep` for faster searching
- Document findings as you go
- Don't fix during audit - just note issues
- Fixes come after full audit complete
