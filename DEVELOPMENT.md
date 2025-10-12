# Hooky Development Workflow

> **RED-GREEN-REFACTOR TDD with Claude Superpowers**

This document outlines the complete development workflow for Hooky, designed for Test-Driven Development using the Claude Superpowers plugin.

## 🏗️ Infrastructure Status

✅ **COMPLETE**: Production-grade development foundation
✅ **COMPLETE**: Comprehensive test harness with deterministic testing
✅ **COMPLETE**: CI/CD pipeline with quality gates
✅ **COMPLETE**: Docker development environment
✅ **COMPLETE**: Database migrations and schema
✅ **COMPLETE**: Documentation structure

**Next Phase**: Begin TDD implementation of webhook ingestion endpoint

## 🎯 TDD Workflow with Claude

### Step 1: Start Development Session

```bash
# Terminal 1: Start TDD watch mode
cargo make tdd

# Terminal 2: Start development services
docker-compose up -d postgres redis

# Terminal 3: Claude development session
# Use Claude Desktop with Superpowers plugin installed
```

### Step 2: RED Phase - Write Failing Test

**Prompt Claude**:
```
Let's implement webhook ingestion using TDD. Start with a RED test for:
- POST /ingest/:endpoint_id accepts JSON webhooks
- Returns 200 OK with event_id
- Persists to PostgreSQL with idempotency

Write the failing integration test first.
```

**Expected Test**:
```rust
#[tokio::test]
async fn webhook_ingestion_accepts_post() {
    let env = TestEnv::new().await.unwrap();

    let response = reqwest::Client::new()
        .post("http://localhost:8080/ingest/test-endpoint")
        .header("Content-Type", "application/json")
        .header("X-Idempotency-Key", "test-123")
        .json(&json!({"event": "test.webhook"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let body: Value = response.json().await.unwrap();
    assert!(body["event_id"].as_str().is_some());
}
```

**Run Test** (should fail):
```bash
cargo test webhook_ingestion_accepts_post
# ❌ Connection refused - RED phase
```

### Step 3: GREEN Phase - Make Test Pass

**Prompt Claude**:
```
The test is failing (RED). Now implement the minimum code to make it pass:
1. Create HTTP server with Axum
2. Add POST /ingest/:endpoint_id route
3. Return 200 OK with mock event_id

Keep it simple - just enough to turn the test GREEN.
```

**Expected Implementation**:
```rust
// src/main.rs
use axum::{extract::Path, routing::post, Json, Router};
use serde_json::{json, Value};

async fn ingest_webhook(
    Path(endpoint_id): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "event_id": "evt_mock_123",
        "status": "received"
    }))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/ingest/:endpoint_id", post(ingest_webhook));

    axum::Server::bind(&"0.0.0.0:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
```

**Run Test** (should pass):
```bash
cargo test webhook_ingestion_accepts_post
# ✅ Test passes - GREEN phase
```

### Step 4: REFACTOR Phase - Improve Design

**Prompt Claude**:
```
Test is now GREEN. Let's refactor to add proper structure:
1. Extract handlers into separate module
2. Add proper error handling with thiserror
3. Add request validation
4. Add database persistence (keep tests passing)

Refactor safely with the test as our safety net.
```

### Step 5: Add Next Test (RED)

**Prompt Claude**:
```
Now let's add the next failing test for idempotency:
- Same webhook sent twice should return same event_id
- Second request should be 200 OK but not create duplicate

Write the test first, then implement.
```

## 🧪 Test Infrastructure Usage

### Available Test Utilities

```rust
use test_harness::{
    TestEnv,                    // Complete test environment
    fixtures::WebhookBuilder,   // Test data builders
    http::MockServer,          // HTTP mocking
    time::TestClock,           // Deterministic time
    ScenarioBuilder,           // Complex test scenarios
};

// Create test environment
let env = TestEnv::new().await?;

// Build test data
let webhook = WebhookBuilder::with_defaults()
    .json_body(json!({"event": "test"}))
    .header("X-Signature", "valid_sig")
    .build();

// Control time deterministically
env.clock.advance(Duration::from_secs(60));

// Mock external HTTP services
env.http_mock
    .mock_endpoint(MockEndpoint::success("/webhook"))
    .await;
```

### Test Categories

1. **Unit Tests** (`cargo test --lib`)
   - Pure logic, no I/O
   - Run in < 1ms each
   - Test individual functions

2. **Integration Tests** (`cargo test --test`)
   - Real database connections
   - HTTP mocking with wiremock
   - Test component interactions

3. **Chaos Tests** (`cargo test chaos_`)
   - Deterministic failure injection
   - Network partitions, timeouts
   - Recovery scenarios

4. **Property Tests** (built into fixtures)
   - Generate random valid inputs
   - Test invariants across wide ranges

## 🔄 Development Commands

```bash
# TDD workflow
cargo make tdd              # Watch tests + clippy
cargo make test-watch       # Watch tests only
cargo make test-unit        # Fast unit tests
cargo make test-integration # Integration tests

# Quality checks
cargo make check            # Full CI pipeline locally
cargo make lint             # Clippy with strict rules
cargo make format           # Auto-format code

# Database
cargo make db-setup         # Create test database
cargo make db-reset         # Reset and re-migrate
cargo make db-migrate       # Run new migrations

# Development server
cargo make dev              # Auto-reload on changes
docker-compose up -d        # Start supporting services
```

## 📋 Feature Implementation Checklist

For each new feature, follow this checklist:

### 1. Design Phase (5 minutes)
- [ ] **Define API**: What endpoints/functions?
- [ ] **Identify edge cases**: What can go wrong?
- [ ] **Choose test strategy**: Unit vs integration vs both?

### 2. RED Phase (10 minutes)
- [ ] **Write failing test**: Test the behavior you want
- [ ] **Run test**: Confirm it fails for the right reason
- [ ] **Document why it fails**: What's missing?

### 3. GREEN Phase (15 minutes)
- [ ] **Implement minimum code**: Just enough to pass
- [ ] **Run test**: Confirm it passes
- [ ] **Run all tests**: Ensure no regressions

### 4. REFACTOR Phase (10 minutes)
- [ ] **Improve code quality**: Extract functions, add types
- [ ] **Add error handling**: Use Result types
- [ ] **Run tests again**: Ensure still passing

### 5. Complete Phase (5 minutes)
- [ ] **Run quality checks**: `cargo make check`
- [ ] **Update documentation**: If public API changed
- [ ] **Commit with message**: `feat(scope): description`

## 🎯 Current Implementation Priority

Based on our roadmap, implement features in this order:

### Phase 1: Core Engine (Current)
1. **Webhook Ingestion Endpoint**
   - POST /ingest/:endpoint_id
   - Idempotency enforcement
   - Request validation

2. **Database Persistence**
   - Insert webhook_events
   - Handle duplicates gracefully
   - Transaction safety

3. **Basic Retry Logic**
   - Background worker pool
   - Exponential backoff
   - Delivery attempts tracking

4. **Health Endpoints**
   - GET /health/live
   - GET /health/ready
   - Database connectivity check

### Phase 2: API & Dashboard (Next)
- REST API for endpoint management
- Simple web dashboard
- Event listing and inspection

## 🐛 Debugging Workflow

When tests fail:

1. **Read the error message carefully**
   ```bash
   cargo test failing_test_name -- --nocapture
   ```

2. **Use deterministic debugging**
   ```bash
   # Replay exact scenario with same seed
   CHAOS_SEED=12345 cargo test chaos_test_name
   ```

3. **Enable detailed logging**
   ```bash
   RUST_LOG=debug,hooky=trace cargo test
   ```

4. **Isolate the problem**
   - Run single test: `cargo test test_name`
   - Check similar tests: `cargo test substring`
   - Verify infrastructure: `cargo test test_infrastructure`

## 🚀 Deployment Pipeline

Our CI/CD ensures quality:

```bash
# Local pre-commit (automatic)
cargo fmt --check
cargo clippy -- -D warnings
cargo test --lib

# CI pipeline (GitHub Actions)
- Format check
- Clippy with pedantic lints
- Full test suite with real PostgreSQL
- Security audit
- Documentation check
- Coverage reporting

# Deployment readiness
cargo make build              # Release binary
docker build -t hooky .       # Container image
cargo make db-migrate         # Schema updates
```

## 📚 Learning Resources

- **TDD Reference**: [Test-Driven Development by Example](https://www.oreilly.com/library/view/test-driven-development/0321146530/)
- **Rust Testing**: [The Rust Book - Testing](https://doc.rust-lang.org/book/ch11-00-testing.html)
- **Axum Web Framework**: [Axum Documentation](https://docs.rs/axum/latest/axum/)
- **Superpowers Plugin**: [Superpowers Repository](https://github.com/obra/superpowers)

## 🎉 Success Metrics

Track these metrics as you develop:

- **Test Coverage**: > 80% overall, 100% for critical paths
- **Test Speed**: Unit tests < 1ms, integration < 100ms
- **Build Time**: < 30 seconds for incremental builds
- **Feedback Loop**: < 5 seconds from save to test result

---

**Remember**: The infrastructure is ready. Now it's time to build something amazing with confidence, one failing test at a time.

**Next Command**: `cargo make tdd` and start your first TDD cycle with Claude!
