# kapsel

> [!WARNING]
> Kapsel is in a pre-release, alpha stage.
>
> The API is unstable, features are incomplete, and breaking changes should be expected.

## The Problem

Webhooks fail. When they do, they fail silently.

Network timeouts, server errors, and rate limits lead to lost events, out-of-sync data, and a complete lack of a verifiable audit trail. You're left with no record and, worse, no proof of delivery.

## The Hook

Kapsel is a webhook reliability service for building guaranteed at-least-once delivery systems.

- **Zero Loss** - Every webhook persisted before acknowledgment
- **Exactly Once Processing** - Database-enforced idempotency
- **At-Least Once Delivery** - Exponential backoff with circuit breakers
- **Cryptographic Proof** - Merkle tree attestation with Ed25519 signatures

## Development

This project uses [Docker](https://www.docker.com/), [cargo-make](https://github.com/sagiegurari/cargo-make) and [nextest](https://github.com/nextest-rs/nextest).

Read the [Makefile](Makefile.toml) for all avaliable tasks.

```bash
# Clone this repository
git clone https://github.com/kapsel-hq/kapsel
cd kapsel

# Optional: Install build tooling
cargo install cargo-make nextest

# Run tests (requires docker test database)
cargo make db-start
cargo make test
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Attestation](docs/ATTESTATION.md)
- [Testing Strategy](docs/TESTING_STRATEGY.md)
- [Style Guide](docs/STYLE.md)

## License

Licensed under the [`Apache License, Version 2.0`](LICENSE)
