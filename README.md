# kapsel

Kapsel is a webhook reliability service that guarantees at-least-once delivery with exponential backoff, circuit breakers, and cryptographically verifiable audit trails.

## What are we building?

Webhook failures are silent killers in distributed systems. Network timeouts, server errors, rate limits, and cascading failures during load spikes lead to lost data and poor user experience. Kapsel solves this by acting as a reliable intermediary that accepts webhooks and guarantees their delivery to destination endpoints.

### Key Features

- **Guaranteed Delivery**: At-least-once delivery with configurable retry policies
- **Idempotency**: Built-in deduplication prevents duplicate processing
- **Circuit Breakers**: Per-endpoint failure isolation to prevent cascading failures
- **Audit Trail**: Complete delivery tracking with TigerBeetle integration
- **Observability**: Structured logging and distributed tracing

## Quick Start

### Prerequisites

- Rust 1.75+
- PostgreSQL 14+

### Build and Run

```bash
# Clone and build
git clone /path/to/kapsel
cd kapsel
cargo build --release

# Set up database
export DATABASE_URL="postgresql://postgres:postgres@localhost:5432/kapsel"
createdb kapsel

# Run server
cargo run
```

### Configuration

Set these environment variables:

```bash
DATABASE_URL=postgresql://postgres:postgres@localhost:5432/kapsel
SERVER_ADDR=127.0.0.1:8080
DATABASE_MAX_CONNECTIONS=10
RUST_LOG=info,kapsel=debug
```

## Development

### Project Structure

```
crates/
├── kapsel-core/          # Domain models and error types
├── kapsel-api/           # HTTP server and handlers
├── test-harness/        # Testing infrastructure
└── ...

docs/                    # Documentation
tests/                   # Integration tests
src/main.rs             # Server entry point
```

### Building

```bash
# Development build
cargo build

# Release build
cargo build --release

# Run with auto-reload
cargo watch -x run
```

### Testing

```bash
# Unit tests (no external dependencies)
cargo test --lib --all

# Integration tests (requires Docker)
cargo test --features docker

# All tests with coverage
cargo test --all --features docker
```

The test suite includes:

- **Unit tests**: Pure logic testing with no I/O
- **Integration tests**: End-to-end testing with real database
- **Property tests**: Invariant validation with generated inputs

Docker-dependent tests are gated behind the `docker` feature flag. This allows CI environments and developers without Docker to run the core test suite.

### Code Quality

```bash
# Format code
cargo fmt

# Run lints
cargo clippy -- -D warnings

# Check everything
cargo fmt --check && cargo clippy -- -D warnings && cargo test --all
```

The codebase follows strict quality standards:

- **No panics**: `unwrap()`, `expect()`, and `panic!()` are forbidden in production code
- **Comprehensive error handling**: All error paths return `Result` types
- **Documentation**: All public APIs have rustdoc comments
- **Consistent style**: See `docs/STYLE.md` for conventions

### Architecture

Kapsel uses a multi-layered architecture:

1. **HTTP Layer** (`kapsel-api`): Axum-based web server with middleware
2. **Domain Layer** (`kapsel-core`): Core business logic and types
3. **Persistence Layer**: PostgreSQL for operations, TigerBeetle for audit
4. **Delivery Layer**: Background workers with retry and circuit breaking

Key design decisions:

- **Strong typing**: Newtype wrappers prevent ID confusion at compile time
- **Zero-copy**: `Bytes` type for efficient payload handling
- **Async throughout**: Tokio-based async runtime
- **Database-first durability**: Write to PostgreSQL before acknowledging requests

### Testing Philosophy

All development follows Test-Driven Development (TDD):

1. **RED**: Write a failing test first
2. **GREEN**: Write minimal code to make it pass
3. **REFACTOR**: Improve code with tests as safety net

The test harness provides:

- Isolated PostgreSQL containers for each test
- HTTP mock servers for external service simulation
- Deterministic time control for retry testing
- Rich fixture builders for test data

## API

### Webhook Ingestion

```http
POST /ingest/:endpoint_id
Content-Type: application/json
X-Idempotency-Key: unique-event-id

{
  "event": "payment.completed",
  "data": {...}
}
```

Response:

```json
{
  "event_id": "evt_1234567890abcdef",
  "status": "received"
}
```

Error responses include structured error codes:

```json
{
  "error": {
    "code": "E1002",
    "message": "Payload too large: size 11000000 bytes exceeds 10MB limit"
  }
}
```

## Error Handling

Kapsel implements a comprehensive error taxonomy:

- **E1001-E1005**: Application errors (invalid signatures, rate limits)
- **E2001-E2005**: Delivery errors (timeouts, HTTP errors, circuit breakers)
- **E3001-E3004**: System errors (database unavailable, queue full)

Each error includes:

- Structured error codes for programmatic handling
- Human-readable messages for debugging
- Retry classification (retryable vs permanent failures)

## Performance

Current targets:

- **Ingestion latency**: < 50ms p99
- **Throughput**: 10K webhooks/second
- **Success rate**: > 99.9%
- **Memory usage**: < 100MB at idle

## Documentation

- `docs/OVERVIEW.md`: System architecture and design
- `docs/SPECIFICATION.md`: Detailed technical requirements
- `docs/STYLE.md`: Code style and conventions
- `docs/development/`: Development guides and ADRs
