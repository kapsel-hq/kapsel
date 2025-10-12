# Kapsel Documentation

> **Bank-grade webhook reliability for mission-critical integrations**

This documentation represents the complete technical and operational specification for Kapsel, our webhook reliability service. Every design decision, architectural choice, and operational procedure is documented here.

## Navigation

### Core Documents

- **[System Overview](./OVERVIEW.md)** - High-level architecture and design philosophy
- **[Technical Specification](./SPECIFICATION.md)** - Complete technical requirements and constraints
- **[Style Guide](./STYLE.md)** - Code style, naming conventions, and engineering standards

### Architecture

- **[Design Principles](./architecture/PRINCIPLES.md)** - Core architectural tenets and trade-offs
- **[Component Architecture](./architecture/COMPONENTS.md)** - Detailed component design and interactions
- **[Data Model](./architecture/DATA_MODEL.md)** - Database schema, data flow, and consistency model
- **[Security Architecture](./architecture/SECURITY.md)** - Security model, threat analysis, and mitigations

**Decision Records:**

- **[ADR-001: Two-Phase Persistence](./architecture/decisions/ADR-001-two-phase-persistence.md)**
- **[ADR-002: TigerBeetle for Audit Log](./architecture/decisions/ADR-002-tigerbeetle-audit.md)**
- **[ADR-003: PostgreSQL for State](./architecture/decisions/ADR-003-postgresql-state.md)**
- **[ADR-004: Deterministic Testing](./architecture/decisions/ADR-004-deterministic-testing.md)**

### Development

- **[Getting Started](./development/GETTING_STARTED.md)** - Local setup and development workflow
- **[Testing Strategy](./development/TESTING.md)** - Testing philosophy and implementation
- **[Chaos Engineering](./development/CHAOS.md)** - Deterministic simulation testing framework
- **[Performance Guide](./development/PERFORMANCE.md)** - Profiling, benchmarking, and optimization

### Operations

- **[Production Runbook](./operations/RUNBOOK.md)** - Operational procedures and incident response
- **[Monitoring & Observability](./operations/MONITORING.md)** - Metrics, logs, and traces
- **[Deployment Guide](./operations/DEPLOYMENT.md)** - Infrastructure and deployment procedures
- **[Disaster Recovery](./operations/DISASTER_RECOVERY.md)** - Backup, restore, and failure scenarios

### Product

- **[Roadmap](./product/ROADMAP.md)** - Phased feature delivery plan
- **[Market Analysis](./product/MARKET.md)** - Target market and competitive positioning
- **[User Experience](./product/UX.md)** - Dashboard and developer experience design

### Reference

- **[API Specification](./reference/API.md)** - Public REST API v1 specification
- **[Configuration Reference](./reference/CONFIG.md)** - Environment variables and configuration
- **[Error Codes](./reference/ERRORS.md)** - Complete error taxonomy
- **[Glossary](./reference/GLOSSARY.md)** - Domain terminology and definitions

## Philosophy

This documentation embodies our engineering philosophy:

1. **Documentation is code** - It's version controlled, reviewed, and never allowed to rot
2. **Design by contract** - Every component has clear inputs, outputs, and invariants
3. **Explicit over implicit** - All assumptions and trade-offs are documented
4. **Testability by design** - If it can't be tested, it doesn't exist

## Contributing

Documentation changes follow the same review process as code:

1. Branch from `main`
2. Make changes with clear, atomic commits
3. Ensure internal links work
4. Submit PR with rationale

## Document Standards

- **Precision**: Technical terms are used precisely. See [Glossary](./reference/GLOSSARY.md)
- **Completeness**: Every significant decision has a rationale
- **Clarity**: Complex concepts include examples
- **Currency**: Documentation is updated alongside code changes

---

_"The code is the truth, but the documentation is the map to understanding it."_
