# Architecture Principles

These principles guide every architectural decision in Kapsel. They are not aspirations—they are requirements. Every component, every abstraction, every line of code must conform to these tenets.

## Core Principles

### 1. Correctness by Construction

**Principle**: Make bugs impossible through design, not discovery.

We use Rust's type system as our first line of defense. If a state is invalid, it must be unrepresentable. If an operation can fail, it must return a Result. If a resource needs cleanup, it must use RAII.

**Implementation**:

- Newtype wrappers for all identifiers (no raw UUIDs)
- State machines encoded in the type system
- Builder patterns for complex configuration
- Compile-time verification over runtime validation

**Example**:

```rust
// BAD: String confusion possible
fn deliver_webhook(endpoint_id: String, event_id: String) { }

// GOOD: Type system prevents mistakes
fn deliver_webhook(endpoint_id: EndpointId, event_id: EventId) { }
```

### 2. Deep Modules

**Principle**: Simple interfaces hiding complex implementations.

A module's interface should be much simpler than its implementation. The cost of using a module should be the time to learn its interface, not the time to understand its internals.

**Implementation**:

- Public API has <10 endpoints while handling 100+ edge cases internally
- Retry logic is completely hidden behind a simple "deliver" interface
- Circuit breaker state management invisible to callers
- Database connection pooling transparent to application code

**Anti-patterns**:

- Shallow modules that just pass through calls
- Interfaces that expose implementation details
- Configuration that requires understanding internals

### 3. Data-Oriented Design

**Principle**: Structure data for the access patterns, not the domain model.

Data layout determines performance. We organize data based on how it's accessed, not how it's conceptually related. Hot data and cold data must be separated. Allocations must be minimized.

**Implementation**:

- Separate tables for frequently-accessed metadata and rarely-accessed payloads
- Columnar storage for append-only audit logs
- Denormalized read models for query performance
- Pre-allocated pools for predictable memory usage

**Trade-offs**:

- Accept controlled denormalization for read performance
- Trade storage space for computation time
- Prefer batch operations over individual queries

### 4. Deterministic by Default

**Principle**: Non-determinism is isolated and mockable.

The core logic must be deterministic and testable. All sources of non-determinism (time, randomness, I/O) must be abstracted behind traits that can be controlled in tests.

**Implementation**:

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub trait HttpClient: Send + Sync {
    async fn post(&self, request: Request) -> Result<Response>;
}

pub trait Rng: Send + Sync {
    fn gen_range(&mut self, range: Range<u64>) -> u64;
}
```

**Benefits**:

- Reproducible test failures
- Time-travel debugging
- Chaos testing without external dependencies
- Fast tests (no real I/O or sleeping)

### 5. Structured Concurrency

**Principle**: Every spawned task has a parent responsible for its lifecycle.

No task runs unsupervised. Every spawned task must be tracked, awaited, and cleanly shut down. Resource leaks from orphaned tasks are unacceptable.

**Implementation**:

- JoinSet for managing multiple tasks
- Explicit abort handles for cancellation
- Graceful shutdown propagation
- select! with biased ordering for shutdown priority

**Invariants**:

- No task outlives its parent
- All tasks respond to shutdown signals
- Resource cleanup happens in reverse order of acquisition

### 6. Backpressure Through Bounded Resources

**Principle**: The system must degrade gracefully under load.

Unbounded growth leads to catastrophic failure. Every queue, every buffer, every pool must have a defined limit. When limits are reached, backpressure must flow upstream.

**Implementation**:

- Bounded channels between components
- Fixed-size connection pools
- Rate limiting at ingestion
- Load shedding when at capacity

**Behavior Under Load**:

- Return 503 when channel is full (don't drop silently)
- Use exponential backoff when resources exhausted
- Maintain fairness through round-robin or fair queuing
- Monitor saturation as a key metric

### 7. Observable Without Overhead

**Principle**: Instrumentation is not optional, but it must not impact performance.

Every significant operation must be observable, but the act of observation must not materially impact the system's performance or behavior.

**Implementation**:

- Structured logging with zero-allocation when disabled
- Metrics as close to the source as possible
- Sampling for high-frequency events
- Trace context propagation throughout

**Standards**:

- Every error includes context
- Every async operation has a span
- Every state transition is logged
- Every queue depth is gauged

### 8. Fail Fast, Recover Faster

**Principle**: Partial failure is normal; total failure is unacceptable.

Individual components will fail. The system must continue operating. Failures must be detected quickly, isolated immediately, and recovered from automatically.

**Implementation**:

- Circuit breakers prevent cascade failures
- Health checks detect failures quickly
- Supervisor trees restart failed components
- Bulkheads isolate failures

**Recovery Strategy**:

- Exponential backoff with jitter
- State recovery from persistent storage
- Reconciliation loops for eventual consistency
- Automatic retry with idempotency

## Architectural Trade-offs

### Consistency vs Availability

**Decision**: We choose availability with eventual consistency.

- PostgreSQL provides immediate operational consistency
- TigerBeetle provides eventual audit consistency
- Reconciliation loops handle the gap
- Accept temporary inconsistency for continuous operation

### Latency vs Throughput

**Decision**: We optimize for throughput with bounded latency.

- Batching improves throughput but increases latency
- We batch with time bounds (max 100ms)
- Individual requests can bypass batching if needed
- SLOs define acceptable latency bounds

### Simplicity vs Flexibility

**Decision**: We choose simplicity and say no to complexity.

- Fixed retry policy rather than custom expressions
- Predefined signature algorithms rather than pluggable
- Structured events rather than arbitrary schemas
- Convention over configuration

### Performance vs Maintainability

**Decision**: We choose maintainability, then optimize hotspots.

- Clear code first, then profile
- Optimize based on measurements, not assumptions
- Document all optimizations and their rationale
- Maintain benchmarks to prevent regression

## Design Patterns

### Repository Pattern

Isolate data access behind trait boundaries:

```rust
#[async_trait]
pub trait EventRepository {
    async fn insert(&self, event: NewEvent) -> Result<EventId>;
    async fn get(&self, id: EventId) -> Result<Option<Event>>;
    async fn claim_pending(&self, limit: usize) -> Result<Vec<Event>>;
}
```

### Command/Query Separation

Write operations return minimal data; read operations are side-effect free:

```rust
// Command: changes state, returns ID only
async fn create_endpoint(&self, cmd: CreateEndpoint) -> Result<EndpointId>;

// Query: no state change, returns full data
async fn get_endpoint(&self, query: GetEndpoint) -> Result<Endpoint>;
```

### Builder Pattern for Complex Objects

```rust
let webhook = WebhookEvent::builder()
    .endpoint_id(endpoint_id)
    .headers(headers)
    .body(body)
    .idempotency_key(key)
    .build()?; // Validates at build time
```

## Anti-Patterns to Avoid

### Anemic Domain Models

Don't create data structures without behavior. Encapsulate operations with the data they operate on.

### Distributed Monolith

Don't split into services prematurely. A well-structured monolith is better than a poorly-structured distributed system.

### Abstraction Without Purpose

Don't create abstractions for single use cases. Abstractions should simplify, not reorganize.

### Defensive Copying

Trust the type system. If ownership is correct, defensive copies are waste.

### Shared Mutable State

Message passing > Locks. When locks are necessary, minimize critical sections.

## Decision Framework

When making architectural decisions, ask:

1. **Does this make the system more correct?**
   - Fewer possible error states?
   - Stronger compile-time guarantees?

2. **Does this make the system simpler?**
   - Fewer concepts to understand?
   - Less surface area?

3. **Does this improve observability?**
   - Easier to debug failures?
   - Better operational visibility?

4. **Is this premature optimization?**
   - Do we have measurements?
   - Is this a demonstrated bottleneck?

5. **Can we defer this decision?**
   - What's the cost of change later?
   - What do we learn by waiting?

## Evolution

These principles are not immutable laws but considered positions. They can evolve, but changes require:

1. Demonstrated failure of current principle
2. Proposed alternative with clear benefits
3. Migration path for existing code
4. Team consensus on the change

Document all principle changes in Architecture Decision Records (ADRs).
