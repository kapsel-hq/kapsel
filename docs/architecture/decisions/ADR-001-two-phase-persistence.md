# ADR-001: Two-Phase Persistence Model

## Status

Accepted

## Context

Webhook ingestion requires both durability guarantees and reasonable latency, while also requiring cryptographically verifiable audit trails for compliance and debugging. These requirements must be balanced:

1. **Zero data loss** - Must never lose a webhook after acknowledging receipt
2. **Acceptable latency** - Sources expect acknowledgment within reasonable time (< 50ms)
3. **Audit integrity** - Need immutable, verifiable event log
4. **Operational flexibility** - Need mutable state for retries and status

A single storage system cannot optimize for all these requirements simultaneously. TigerBeetle provides cryptographic verification but requires batching for efficiency. PostgreSQL provides immediate durability and flexible querying but lacks immutability guarantees.

## Decision

Implement a two-phase persistence model:

**Phase 1: Operational Persistence (PostgreSQL)**

- Write to PostgreSQL with status `received` BEFORE returning 200 OK
- Provides absolute durability guarantee - no data loss window
- Enables immediate querying and state management
- Ingestion latency increases but remains acceptable (p99 < 50ms)

**Phase 2: Audit Logging (TigerBeetle)**

- Asynchronously append to TigerBeetle after acknowledgment
- Provides cryptographic proof and immutability
- Fully decoupled from ingestion path - zero latency impact
- Can be batched for maximum efficiency

**Reconciliation**

- Background process ensures all events in PostgreSQL eventually reach TigerBeetle
- Events remain in `received` state until TigerBeetle confirms
- Transition to `pending` state enables delivery
- No data loss risk since PostgreSQL write happens before acknowledgment

## Consequences

### Positive

- **Zero data loss window** - PostgreSQL commit before acknowledgment is absolute
- **Acceptable ingestion latency** - PostgreSQL writes complete in < 50ms
- **Cryptographic audit trail** - TigerBeetle provides non-repudiation
- **Operational flexibility** - Can query, update, and manage state in PostgreSQL
- **Graceful degradation** - System continues working if TigerBeetle is temporarily unavailable
- **Simple error handling** - If PostgreSQL write fails, return 503 and source retries

### Negative

- **Eventual consistency** - Audit log lags behind operational state
- **Additional complexity** - Must manage two storage systems
- **Reconciliation overhead** - Background process required for consistency
- **Storage duplication** - Events stored twice (acceptable trade-off)

### Neutral

- **Operational primacy** - PostgreSQL is source of truth for operations, TigerBeetle for audit
- **Batch efficiency** - Can batch TigerBeetle writes without impacting latency
- **Recovery semantics** - Must recover from both systems on restart

## Implementation Details

### State Transitions

```
HTTP POST -> PostgreSQL INSERT (received) -> 200 OK -> TigerBeetle append -> pending -> delivering -> delivered/failed
```

The critical point: 200 OK is returned only AFTER the PostgreSQL INSERT commits.

### Failure Modes

**PostgreSQL Write Fails**

- Return 503 Service Unavailable immediately
- No data written anywhere
- Source will retry
- This is the ONLY scenario where we return an error

**TigerBeetle Write Fails**

- Event remains in `received` state
- Reconciliation will retry
- Delivery blocked until logged

**Reconciliation Fails**

- Events accumulate in `received` state
- Monitoring alert triggered
- Manual intervention if persistent

### Database Schema

PostgreSQL:

```sql
CREATE TABLE webhook_events (
    id UUID PRIMARY KEY,
    status TEXT NOT NULL, -- 'received', 'pending', 'delivered', 'failed'
    tigerbeetle_id UUID, -- NULL until logged
    -- ... other fields
);

CREATE INDEX idx_unlogged ON webhook_events(status)
    WHERE status = 'received';
```

TigerBeetle:

```rust
struct WebhookAuditEntry {
    id: u128,
    event_hash: [u8; 32],
    // ... immutable audit fields
}
```

## Alternatives Considered

### Single Database (PostgreSQL Only)

- **Pros**: Simpler, single source of truth
- **Cons**: No cryptographic guarantees, can be modified

### In-Memory Queue Before Persistence

- **Pros**: Very low latency (< 10ms)
- **Cons**: Data loss window between acknowledgment and persistence
- **Rejected**: Unacceptable for a reliability service

### Single Database (TigerBeetle Only)

- **Pros**: Immutable, cryptographically verifiable
- **Cons**: No flexible querying, cannot update retry state

### Synchronous Dual-Write

- **Pros**: Consistent view across both stores
- **Cons**: High latency, complex failure handling

### Write-Ahead Log Pattern

- **Pros**: Could use PostgreSQL WAL for durability
- **Cons**: Complex recovery, ties us to PostgreSQL internals

## References

- [TigerBeetle Design Document](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/DESIGN.md)
- [PostgreSQL SKIP LOCKED](https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE)
- [Two-Phase Commit Protocol](https://en.wikipedia.org/wiki/Two-phase_commit_protocol)
