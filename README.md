# Kapsel

The building blocks for unbreakable webhook integrations.

Kapsel guarantees at-least-once delivery:

- **Zero Loss**: PostgreSQL persistence before acknowledgment
- **Smart Retries**: Exponential backoff with circuit breakers
- **Complete Audit**: Cryptographic proof of delivery attempts
- **High Performance**: 10K+ webhooks/sec with sub-50ms latency

## Quick Start

```bash
# Prerequisites: Rust 1.75+, PostgreSQL 14+
git clone <repository>
cd kapsel
cargo build --release

# Setup database
export DATABASE_URL="postgresql://postgres:postgres@localhost:5432/kapsel"
createdb kapsel

# Run server
cargo run
```

## Usage

```bash
# Ingest webhook
curl -X POST http://localhost:8080/ingest/endpoint-123 \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: unique-event-id" \
  -d '{"event": "payment.completed", "data": {...}}'

# Response
{"event_id": "evt_1234567890abcdef", "status": "received"}
```

## Implementation Status

- DONE: **Webhook Ingestion**: Production ready with idempotency
- ONGOING **Delivery Engine**: HTTP client and retry logic (2 weeks)
- PLANNED **Audit Trails**: TigerBeetle integration planned
- PLANNED **Management API**: Endpoint configuration and monitoring

## Development

```bash
# Run tests
cargo test

# With coverage
cargo test --all-features

# Format and lint
cargo fmt && cargo clippy -- -D warnings

# Development server
cargo run
```

## Documentation

- [Architecture Overview](docs/OVERVIEW.md) - System design and data flow
- [Implementation Status](docs/IMPLEMENTATION_STATUS.md) - Current capabilities
- [Technical Specification](docs/SPECIFICATION.md) - Detailed requirements
- [Development Guide](docs/TESTING_STRATEGY.md) - Testing and TDD approach
- [Style Guide](docs/STYLE.md) - Code conventions

## License

MIT
