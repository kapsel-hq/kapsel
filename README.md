<p align="center">
  <img width="60%" src="docs/images/logo.png" alt="LOGO Screenshot">
</p>

<p align="center">
  <b><a href="docs/STATUS.md">Status</a></b>
  &nbsp;|&nbsp;
  <b><a href="docs/ARCHITECTURE.md">Architecture</a></b>
  &nbsp;|&nbsp;
  <b><a href="docs/ATTESTATION.md">Attestation</a></b>
  &nbsp;|&nbsp;
  <b><a href="docs/TESTING_STRATEGY.md">Testing Strategy</a></b>
  &nbsp;|&nbsp
  <b><a href="docs/STYLE.md">Style Guide</a></b>
</p>

> [!WARNING]
> Kapsel is in early development phase.
>
> The API is unstable, features are incomplete, and breaking changes should be expected.

## The Hook

Kapsel is a webhook reliability service for building guaranteed at-least-once delivery systems.

- **Zero Loss** - Every webhook persisted before acknowledgment
- **Exactly Once Processing** - Database-enforced idempotency
- **At-Least Once Delivery** - Exponential backoff with circuit breakers
- **Cryptographic Proof** - Merkle tree attestation with Ed25519 signatures

## Development

```bash
git clone https://github.com/kapsel-hq/kapsel
cd kapsel

# Development
cargo make build
cargo make tdd

# Testing
cargo make test

# Run all checks
cargo make check

# Database
cargo make db-setup
cargo make db-reset
```

Read `Makefile.toml` for all avaliable `cargo-make` tasks.

## License

Licensed under the Apache License, Version 2.0.

---

See [`docs/`](docs/) for detailed design, development guide, and testing philosophy.
