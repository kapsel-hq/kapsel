# Component Architecture

This document details the internal architecture of each major component in Kapsel. Each component is described with its responsibilities, interfaces, implementation details, and failure modes.

## Component Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    HTTP Ingestion Layer                     │
├──────────┬───────────────────┬──────────────┬───────────────┤
│  Router  │ Request Validator │ Rate Limiter │ Sig Validator │
└──────────┴───────────────────┴──────────────┴───────────────┘
                              │
                              ▼
                 ┌──────────────────────────┐
                 │        PostgreSQL        │
                 │  (Immediate Durability)  │
                 └──────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Post-Ingestion Pipeline                   │
├────────────────────┬─────────────────────┬──────────────────┤
│  Event Processor   │  TigerBeetle Logger │ Reconciliation   │
└────────────────────┴─────────────────────┴──────────────────┘
                              │
                              ▼
                 ┌──────────────────────────┐
                 │        TigerBeetle       │
                 │        (Audit Log)       │
                 └──────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                         Worker Pool                         │
├──────────────┬──────────────┬──────────────┬────────────────┤
│   Scheduler  │   Workers    │ Delivery Eng │ Circuit Break  │
└──────────────┴──────────────┴──────────────┴────────────────┘
```

## HTTP Ingestion Layer

### Router (Axum)

**Responsibility**: HTTP request routing and middleware orchestration.

**Interface**:

```rust
pub struct Router {
    app: axum::Router,
    state: Arc<AppState>,
}

impl Router {
    pub fn new(state: AppState) -> Self;
    pub async fn serve(self, addr: SocketAddr) -> Result<()>;
}
```

**Routes**:

- `POST /ingest/:endpoint_id` - Webhook ingestion endpoint
- `GET /health/live` - Liveness probe
- `GET /health/ready` - Readiness probe
- `GET /metrics` - Prometheus metrics

**Middleware Stack** (executed in order):

1. Request ID injection
2. Trace context extraction
3. Timeout enforcement (30s)
4. Payload size limit (10MB)
5. Rate limiting
6. Signature validation
7. PostgreSQL persistence
8. Request logging

**Error Handling**:

- Catches panics and returns 500
- Maps domain errors to HTTP status codes
- Ensures correlation ID in all error responses

### Request Validator

**Responsibility**: Validate incoming webhook structure and headers.

**Interface**:

```rust
pub struct RequestValidator {
    max_payload_size: usize,
    allowed_content_types: HashSet<ContentType>,
}

impl RequestValidator {
    pub fn validate(&self, request: &Request) -> Result<ValidatedRequest>;
}
```

**Validations**:

1. Content-Type is supported
2. Payload size within limits
3. Required headers present
4. Idempotency key extraction
5. Character encoding validation

**Failure Modes**:

- Returns 413 for oversized payloads
- Returns 415 for unsupported media types
- Returns 400 for malformed requests

### Rate Limiter

**Responsibility**: Enforce per-tenant ingestion limits.

**Interface**:

```rust
pub struct RateLimiter {
    store: Arc<RateLimitStore>,
    default_limit: Rate,
}

impl RateLimiter {
    pub async fn check(&self, tenant_id: TenantId) -> Result<RateLimitDecision>;
    pub async fn record(&self, tenant_id: TenantId, count: u32);
}

pub struct Rate {
    requests: u32,
    per: Duration,
}
```

**Algorithm**: Token bucket with Redis backend

- Refill rate based on subscription tier
- Burst allowance of 2x sustained rate
- Separate limits for ingestion vs API calls

**Response**:

- Returns 429 with Retry-After header
- Includes X-RateLimit-Limit, X-RateLimit-Remaining headers

### Signature Validator

**Responsibility**: Verify webhook authenticity via HMAC-SHA256.

**Interface**:

```rust
pub struct SignatureValidator {
    providers: HashMap<String, Box<dyn SignatureProvider>>,
}

#[async_trait]
pub trait SignatureProvider {
    fn header_name(&self) -> &str;
    async fn validate(&self, secret: &[u8], payload: &[u8], signature: &str) -> Result<()>;
}
```

**Supported Providers**:

- Stripe: `stripe-signature` with timestamp validation
- GitHub: `X-Hub-Signature-256`
- Shopify: `X-Shopify-Hmac-Sha256`
- Generic: `X-Webhook-Signature`

**Security**:

- Constant-time comparison
- Timestamp validation (±5 minutes)
- Secret rotation support

## Channel & Dispatcher

### Event Processor

**Responsibility**: Process events that have been durably persisted to PostgreSQL.

**Implementation**:

```rust
pub struct EventProcessor {
    postgres: Arc<PostgresStore>,
    tigerbeetle: Arc<TigerBeetleClient>,
    interval: Duration,
}

impl EventProcessor {
    pub async fn run(&self) -> Result<()> {
        let mut interval = tokio::time::interval(self.interval);

        loop {
            interval.tick().await;

            // Poll for newly received events
            let events = self.postgres
                .find_by_status("received", 100)
                .await?;

            for event in events {
                // Attempt TigerBeetle logging
                match self.tigerbeetle.append(&event).await {
                    Ok(tb_id) => {
                        // Update to pending for delivery
                        self.postgres
                            .update_status(&event.id, "pending", Some(tb_id))
                            .await?;
                    }
                    Err(e) => {
                        // Log error, reconciliation will retry
                        tracing::warn!("TigerBeetle write failed: {}", e);
                    }
                }
            }
        }
    }
}
```

**Polling Strategy**:

- Poll interval: 100ms
- Batch size: 100 events
- Priority: Oldest events first

**Monitoring**:

- Gauge: Events in "received" state
- Counter: Events transitioned to "pending"
- Histogram: Time in "received" state

### TigerBeetle Logger

**Responsibility**: Asynchronously log events to TigerBeetle for audit trail.

**Implementation**:

```rust
pub struct TigerBeetleLogger {
    postgres: Arc<PostgresStore>,
    tigerbeetle: Arc<TigerBeetleClient>,
    batch_size: usize,
    batch_timeout: Duration,
}

impl TigerBeetleLogger {
    pub async fn run(&self) -> Result<()> {
        let mut batch = Vec::with_capacity(self.batch_size);
        let mut timeout = tokio::time::interval(self.batch_timeout);

        loop {
            select! {
                biased;

                _ = self.shutdown.recv() => {
                    self.flush_batch(&mut batch).await?;
                    break;
                }

                _ = timeout.tick() => {
                    // Fetch events needing TigerBeetle logging
                    let events = self.postgres
                        .find_unlogged(self.batch_size)
                        .await?;

                    if !events.is_empty() {
                        self.log_batch(events).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn log_batch(&self, events: Vec<Event>) -> Result<()> {
        // Batch append to TigerBeetle
        let entries: Vec<_> = events.iter()
            .map(|e| e.to_audit_entry())
            .collect();

        let tb_ids = self.tigerbeetle
            .append_batch(&entries)
            .await?;

        // Update PostgreSQL with TigerBeetle IDs
        for (event, tb_id) in events.iter().zip(tb_ids.iter()) {
            self.postgres
                .update_tigerbeetle_id(&event.id, *tb_id)
                .await?;
        }

        Ok(())
    }
}
```

**Optimization**:

- Batch reads to reduce query overhead
- Batch writes to TigerBeetle for throughput
- Prepared statements for all queries
- Connection pooling for concurrency

### Reconciliation Worker

**Responsibility**: Ensure eventual consistency between PostgreSQL and TigerBeetle.

**Implementation**:

```rust
pub struct ReconciliationWorker {
    postgres: Arc<PostgresStore>,
    tigerbeetle: Arc<TigerBeetleClient>,
    interval: Duration,
}

impl ReconciliationWorker {
    pub async fn run(&self) -> Result<()> {
        let mut interval = tokio::time::interval(self.interval);

        loop {
            interval.tick().await;

            // Find events in 'received' state (not yet in TigerBeetle)
            let pending = self.postgres.find_unlogged(100).await?;

            for event in pending {
                match self.tigerbeetle.append(&event).await {
                    Ok(tb_id) => {
                        self.postgres.mark_logged(&event.id, tb_id).await?;
                    }
                    Err(e) if e.is_duplicate() => {
                        // Already logged, just update reference
                        let tb_id = self.tigerbeetle.find(&event).await?;
                        self.postgres.mark_logged(&event.id, tb_id).await?;
                    }
                    Err(e) => {
                        tracing::warn!("Reconciliation failed for {}: {}", event.id, e);
                    }
                }
            }
        }
    }
}
```

## Worker Pool

### Scheduler

**Responsibility**: Assign delivery tasks to workers efficiently.

**Implementation**:

```rust
pub struct Scheduler {
    workers: JoinSet<()>,
    semaphore: Arc<Semaphore>,
    postgres: Arc<PostgresStore>,
}

impl Scheduler {
    pub async fn run(&self, worker_count: usize) -> Result<()> {
        let semaphore = Arc::new(Semaphore::new(worker_count));

        for worker_id in 0..worker_count {
            let permit = semaphore.clone().acquire_owned().await?;

            self.workers.spawn(async move {
                let _permit = permit; // Dropped on completion
                Worker::new(worker_id).run().await
            });
        }

        // Monitor and restart failed workers
        while let Some(result) = self.workers.join_next().await {
            match result {
                Err(e) => {
                    tracing::error!("Worker died: {}", e);
                    // Restart worker
                    self.spawn_worker().await?;
                }
                Ok(()) => {
                    // Graceful shutdown
                }
            }
        }

        Ok(())
    }
}
```

**Work Distribution**:

- PostgreSQL `FOR UPDATE SKIP LOCKED` for work claiming
- No central queue or coordinator needed
- Fair distribution through random worker wake

### Worker

**Responsibility**: Claim events and manage delivery lifecycle.

**Implementation**:

```rust
pub struct Worker {
    id: WorkerId,
    postgres: Arc<PostgresStore>,
    delivery: Arc<DeliveryEngine>,
}

impl Worker {
    pub async fn run(&self) -> Result<()> {
        loop {
            // Claim batch of events
            let events = self.claim_events(10).await?;

            if events.is_empty() {
                // No work available, backoff
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Process each event
            for event in events {
                let span = tracing::span!(
                    Level::INFO,
                    "deliver",
                    event_id = %event.id,
                    attempt = event.failure_count + 1
                );

                async {
                    match self.delivery.deliver(&event).await {
                        Ok(response) => {
                            self.postgres.mark_delivered(&event.id).await?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                if now - last > self.config.reset_timeout_secs {
                    self.state.store(2, Ordering::Release); // Half-open
                    true
                } else {
                    false
                }
            }
            2 => {
                // Half-open - allow limited requests
                self.success_count.load(Ordering::Acquire) < 3
            }
            _ => unreachable!()
        }
    }

    pub fn record_success(&self) {
        let state = self.state.load(Ordering::Acquire);

        match state {
            0 => {
                // Closed - reset failure count
                self.failure_count.store(0, Ordering::Release);
            }
            2 => {
                // Half-open - check if should close
                let count = self.success_count.fetch_add(1, Ordering::AcqRel) + 1;
                if count >= 3 {
                    self.state.store(0, Ordering::Release); // Close
                    self.failure_count.store(0, Ordering::Release);
                    self.success_count.store(0, Ordering::Release);
                }
            }
            _ => {}
        }
    }

    pub fn record_failure(&self) {
        let state = self.state.load(Ordering::Acquire);

        match state {
            0 => {
                // Closed - check if should open
                let count = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
                if count >= self.config.failure_threshold {
                    self.state.store(1, Ordering::Release); // Open
                    self.last_failure.store(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        Ordering::Release
                    );
                }
            }
            2 => {
                // Half-open - reopen immediately
                self.state.store(1, Ordering::Release);
                self.last_failure.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                    Ordering::Release
                );
            }
            _ => {}
        }
    }
}
```

## Data Stores

### PostgreSQL Store

**Responsibility**: Operational state management and work queue.

**Connection Pool Configuration**:

```rust
pub struct PostgresConfig {
    max_connections: 100,
    min_connections: 10,
    acquire_timeout: Duration::from_secs(5),
    idle_timeout: Duration::from_secs(600),
    max_lifetime: Duration::from_secs(1800),
}
```

**Key Operations**:

- Batch insert with COPY for high throughput
- Advisory locks for distributed coordination
- SKIP LOCKED for work distribution
- Prepared statements for hot paths

### TigerBeetle Client

**Responsibility**: Immutable audit log with cryptographic verification.

**Implementation**:

```rust
pub struct TigerBeetleClient {
    client: tigerbeetle::Client,
    cluster_id: u128,
}

impl TigerBeetleClient {
    pub async fn append(&self, event: &Event) -> Result<TigerBeetleId> {
        let entry = WebhookAuditEntry {
            id: Uuid::new_v4().as_u128(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            tenant_id: event.tenant_id.as_u128(),
            event_hash: sha256(&event),
            flags: 0,
            reserved: [0; 14],
        };

        self.client.create_entries(&[entry]).await?;
        Ok(TigerBeetleId(entry.id))
    }
}
```

## Observability Layer

### Metrics Collector

**Prometheus Metrics**:

```rust
// Counters
webhook_received_total{tenant_id, endpoint_id, status}
webhook_delivered_total{tenant_id, endpoint_id, status}
webhook_retries_total{tenant_id, endpoint_id, reason}

// Histograms
webhook_ingestion_duration_seconds
webhook_delivery_duration_seconds
webhook_queue_time_seconds

// Gauges
webhook_queue_depth{status}
worker_pool_active
circuit_breaker_state{endpoint_id, state}
```

### Trace Propagator

**OpenTelemetry Integration**:

- Extract trace context from incoming webhooks
- Propagate context through async boundaries
- Export to OTLP collector
- Sample at 1% for normal traffic, 100% for errors

### Failure Recovery

### Component Failure Matrix

| Component       | Failure Mode            | Detection        | Recovery                          |
| --------------- | ----------------------- | ---------------- | --------------------------------- |
| HTTP Receiver   | Database write fails    | Immediate error  | Return 503, source retries        |
| Event Processor | TigerBeetle unavailable | Write timeout    | Events remain in "received" state |
| Reconciliation  | Repeated failures       | Monitoring alert | Manual intervention               |

### Ingestion Flow (Durability-First)

1. HTTP request received
2. Validate signature and headers
3. INSERT into PostgreSQL with status "received"
4. Return 200 OK to source
5. Background: Log to TigerBeetle
6. Background: Update status to "pending"
7. Background: Workers claim for delivery

This ensures zero data loss even if the process crashes immediately after returning 200 OK.
| Component | Failure Mode | Detection | Recovery |estion (< 10ms p99) 2. Event claiming query (< 5ms p99) 3. Delivery attempt (< 100ms p50)

### Optimization Techniques

- Zero-copy deserialization with serde_json::from_slice
- Prepared statements for all queries
- Connection pooling with warm connections
- Batch operations where possible
- Lock-free data structures in hot paths

### Capacity Planning

- 10KB average webhook size
- 10K webhooks/sec = 100MB/sec ingestion
- 100 PostgreSQL connections = 1K concurrent queries
- 1K workers = 10K concurrent deliveries (10 per worker)
