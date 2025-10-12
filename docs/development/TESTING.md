# Testing Strategy

Testing is not an afterthought—it's our primary mechanism for building confidence in system correctness. This document defines our comprehensive testing approach, from unit tests to chaos engineering.

## Testing Philosophy

### Principles

1. **Test behavior, not implementation** - Tests should survive refactoring
2. **Fast feedback loops** - Most tests run in milliseconds, not seconds
3. **Deterministic outcomes** - Flaky tests are broken tests
4. **Test at the right level** - Unit for logic, integration for interactions
5. **Production-like testing** - Test with real dependencies where critical

### Test Pyramid

```
         ╱ E2E ╲           (Few: Critical user journeys)
        ╱───────╲
       ╱ Chaos   ╲         (Targeted: Failure scenarios)
      ╱───────────╲
     ╱ Integration ╲       (Balanced: Component interactions)
    ╱───────────────╲
   ╱  Property/Fuzz  ╲     (Extensive: Invariant validation)
  ╱───────────────────╲
 ╱    Unit Tests       ╲    (Many: Pure logic validation)
└───────────────────────┘
```

## Test Categories

### Unit Tests

**Purpose**: Validate individual functions and modules in isolation.

**Approach**:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idempotency_key_extraction() {
        let headers = Headers::new()
            .with("X-Idempotency-Key", "test-key-123");

        let key = extract_idempotency_key(&headers).unwrap();
        assert_eq!(key, IdempotencyKey::from("test-key-123"));
    }

    #[test]
    fn test_exponential_backoff_calculation() {
        let backoff = calculate_backoff(3, Duration::from_secs(1));
        assert!(backoff >= Duration::from_secs(8));
        assert!(backoff <= Duration::from_secs(10)); // With jitter
    }
}
```

**Standards**:

- Test one concern per test
- Use descriptive test names
- Arrange-Act-Assert pattern
- No external dependencies
- Run in < 1ms per test

### Integration Tests

**Purpose**: Validate component interactions with real dependencies.

**Database Tests**:

```rust
#[sqlx::test]
async fn test_event_persistence_and_retrieval() {
    let pool = test_db_pool().await;
    let store = PostgresStore::new(pool);

    // Create test event
    let event = WebhookEvent::builder()
        .endpoint_id(EndpointId::new())
        .headers(test_headers())
        .body(Bytes::from_static(b"test payload"))
        .build()
        .unwrap();

    // Persist
    let id = store.insert(event.clone()).await.unwrap();

    // Retrieve
    let retrieved = store.get(id).await.unwrap().unwrap();
    assert_eq!(retrieved.body, event.body);

    // Verify idempotency
    let duplicate = store.insert(event).await;
    assert!(matches!(duplicate, Err(Error::DuplicateEvent)));
}
```

**HTTP Tests**:

```rust
#[tokio::test]
async fn test_webhook_ingestion_flow() {
    let app = test_app().await;

    let response = app
        .post("/ingest/test-endpoint")
        .header("Content-Type", "application/json")
        .header("X-Idempotency-Key", "test-123")
        .body(json!({"event": "test"}))
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    // Verify event was queued
    let metrics = app.metrics().await;
    assert_eq!(metrics.events_queued, 1);
}
```

### Property-Based Tests

**Purpose**: Verify invariants hold across wide input ranges.

**Setup**:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_idempotency_always_returns_same_result(
        key in "[a-zA-Z0-9-]{1,64}",
        payload in prop::collection
::vec(any::<u8>(), 1..10000)
    ) {
        let event1 = create_event(&key, &payload);
        let event2 = create_event(&key, &payload);

        assert_eq!(event1.id, event2.id);
        assert_eq!(event1.hash, event2.hash);
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold(
        failures in 5..100usize,
        threshold in 1..5usize
    ) {
        let breaker = CircuitBreaker::new(threshold);

        for _ in 0..failures {
            breaker.record_failure();
        }

        if failures >= threshold {
            assert!(!breaker.should_allow_request());
        }
    }
}
```

**Invariants to Test**:

- Idempotency guarantees
- Retry count limits
- Circuit breaker thresholds
- Rate limiting boundaries
- Queue capacity limits

### Fuzz Testing

**Purpose**: Discover security vulnerabilities and edge cases.

**Target Areas**:

```rust
// Fuzz signature validation
#[no_mangle]
pub extern "C" fn fuzz_signature_validation(data: &[u8]) {
    if data.len() < 64 { return; }

    let (secret, signature, payload) = data.split_at(32).split_at(32);

    // Should never panic
    let _ = validate_hmac_sha256(secret, payload, signature);
}

// Fuzz JSON parsing
#[no_mangle]
pub extern "C" fn fuzz_json_parsing(data: &[u8]) {
    // Should handle any input without panic
    let _ = parse_webhook_payload(data);
}
```

**Run with**:

```bash
cargo fuzz run fuzz_signature_validation -- -max_len=10000
```

### Chaos Tests

**Purpose**: Validate system resilience under failure conditions.

**Framework**:

```rust
pub struct DeterministicHarness {
    clock: SimulatedClock,
    http: ScriptedHttpClient,
    rng: SeededRng,
    postgres: EmbeddedPostgres,
    events: Vec<SystemEvent>,
}

impl DeterministicHarness {
    pub async fn run_scenario
(&mut self, script: TestScript) -> TestResult {
        for action in script.actions {
            match action {
                Action::IngestWebhook(webhook) => {
                    self.ingest(webhook).await?;
                }
                Action::AdvanceTime(duration) => {
                    self.clock.advance(duration);
                    self.process_timers().await?;
                }
                Action::InjectFailure(failure) => {
                    self.inject_failure(failure).await?;
                }
                Action::AssertState(assertion) => {
                    self.verify_assertion(assertion)?;
                }
            }
        }

        TestResult::from_events(&self.events)
    }
}
```

**Test Scenarios**:

```rust
#[tokio::test]
async fn test_delivery_retry_with_network_failures() {
    let mut harness = DeterministicHarness::new();

    let script = TestScript::new()
        .ingest_webhook(test_webhook())
        .inject_failure(Failure::NetworkTimeout)
        .advance_time(Duration::from_secs(1))
        .assert_retry_attempted(1)
        .inject_failure(Failure::Http500)
        .advance_time(Duration::from_secs(2))
        .assert_retry_attempted(2)
        .allow_success()
        .advance_time(Duration::from_secs(4))
        .assert_delivered();

    let result = harness.run_scenario(script).await;
    assert!(result.is_success());
    assert_eq!(result.total_attempts, 3);
}
```

### End-to-End Tests

**Purpose**: Validate complete user workflows.

**Example**:

```rust
#[tokio::test]
async fn test_complete_webhook_flow() {
    let env = TestEnvironment::spawn().await;

    // Create endpoint
    let endpoint = env.api_client
        .create_endpoint("test-endpoint", "https://example.com/webhook")
        .await
        .unwrap();

    // Send webhook
    let response = env.http_client
        .post(&endpoint.ingestion_url)
        .json(&json!({"event": "test"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    // Wait for delivery
    env.wait_for_delivery(&endpoint.id, Duration::from_secs(5))
        .await
        .unwrap();

    // Verify in dashboard
    let events = env.api_client
        .list_events(&endpoint.id)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status, "delivered");
}
```

## Test Data Management

### Fixtures

```rust
pub mod fixtures {
    pub fn test_webhook() -> IncomingWebhook {
        IncomingWebhook {
            headers: headers! {
                "Content-Type" => "application/json",
                "X-Webhook-Signature" => "sha256=...",
            },
            body: Bytes::from_static(br#"{"id": "evt_123"}"#),
        }
    }

    pub fn test_endpoint() -> Endpoint {
        Endpoint {
            id: EndpointId::from("ep_test"),
            url: "https://example.com/webhook".parse().unwrap(),
            max_retries: 3,
            timeout: Duration::from_secs(30),
        }
    }
}
```

### Test Builders

```rust
pub struct WebhookEventBuilder {
    endpoint_id: Option<EndpointId>,
    headers: Option<Headers>,
    body: Option<Bytes>,
    idempotency_key: Option<IdempotencyKey>,
}

impl WebhookEventBuilder {
    pub fn with_random_data(self) -> Self {
        self.endpoint_id(EndpointId::new())
            .headers(Headers::random())
            .body(Bytes::random(1024))
            .idempotency_key(IdempotencyKey::random())
    }
}
```

## Test Infrastructure

### Database Testing

```rust
// Automatic transaction rollback
#[sqlx::test]
async fn test_with_db(pool: PgPool) {
    // Test runs in transaction
    // Automatically rolled back after test
}

// Embedded Postgres for integration tests
pub async fn embedded_postgres() -> PgPool {
    let pg = PgEmbed::new()
        .port(0) // Random available port
        .start()
        .await
        .unwrap();

    pg.create_database("test").await.unwrap();
    pg.migrate("./migrations").await.unwrap();
    pg.pool().await.unwrap()
}
```

### HTTP Mocking

```rust
use wiremock::{MockServer, Mock, ResponseTemplate};

#[tokio::test]
async fn test_webhook_delivery_with_mocked_endpoint() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/webhook"))
        .and(header("Content-Type", "application/json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Configure endpoint to use mock_server.uri()
    // Run delivery test
}
```

## Performance Testing

### Benchmarks

```rust
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_signature_validation(c: &mut Criterion) {
    let secret = b"test-secret";
    let payload = vec![0u8; 10_000];
    let signature = calculate_hmac(secret, &payload);

    c.bench_function("hmac_validation", |b| {
        b.iter(|| validate_hmac(secret, &payload, &signature))
    });
}

criterion_group!(benches, bench_signature_validation);
criterion_main!(benches);
```

### Load Testing

```yaml
# k6 load test script
import http from 'k6/http';
import { check } from 'k6';

export let options = {
  stages: [
    { duration: '5m', target: 100 },  // Ramp up
    { duration: '10m', target: 1000 }, // Stay at 1000 RPS
    { duration: '5m', target: 0 },     // Ramp down
  ],
  thresholds: {
    http_req_duration: ['p(99)<10'], // 99% under 10ms
    http_req_failed: ['rate<0.01'],  // Less than 1% errors
  },
};

export default function() {
  let response = http.post('https://api.example.com/ingest/test', {
    event: 'load_test',
  });

  check(response, {
    'status is 200': (r) => r.status === 200,
  });
}
```

## Continuous Integration

### Test Pipeline

```yaml
test:
  stage: test
  parallel:
    matrix:
      - TEST_TYPE: [unit, integration, property]
  script:
    - cargo test --${TEST_TYPE}

security:
  stage: test
  script:
    - cargo fuzz run fuzz_all -- -max_total_time=300

chaos:
  stage: test
  script:
    - cargo test --test chaos -- --nocapture
```

### Coverage Requirements

- Overall: ≥ 80%
- Critical paths: 100%
- New code: ≥ 90%
- Excludes: Generated code, tests

### Test Execution Time Limits

| Test Type   | Max Duration | Timeout |
| ----------- | ------------ | ------- |
| Unit        | 1ms          | 100ms   |
| Integration | 100ms        | 5s      |
| Property    | 1s           | 30s     |
| Chaos       | 10s          | 60s     |
| E2E         | 30s          | 120s    |

## Debugging Failed Tests

### Deterministic Replay

```rust
// When a chaos test fails, it outputs a seed
// Reproduce the exact failure:
CHAOS_SEED=12345 cargo test test_delivery_retry_with_network_failures
```

### Test Observability

```rust
// Enable detailed logging for test debugging
RUST_LOG=debug,kapsel=trace cargo test failing_test -- --nocapture
```

### Time Travel Debugging

```rust
// Use simulated clock to debug timing issues
harness.clock.set(Instant::from_secs(1234));
harness.run_until_event(EventType::RetryScheduled).await;
println!("State at retry: {:?}", harness.snapshot());
```

## Best Practices

### DO

- Write tests before fixing bugs
- Test error paths as thoroughly as success paths
- Use property tests for algorithmic code
- Keep tests independent and isolated
- Document why, not what, in test names

### DON'T

- Use sleep() in tests (use simulated time)
- Share state between tests
- Test private implementation details
- Ignore flaky tests (fix or delete)
- Mock what you don't own

### Test Naming

```rust
// Good: Describes behavior and expectation
#[test]
fn webhook_with_invalid_signature_returns_unauthorized() {}

// Bad: Vague and unhelpful
#[test]
fn test_webhook() {}
```

## Testing Checklist

Before merging any PR:

- [ ] Unit tests for new logic
- [ ] Integration tests for new endpoints
- [ ] Property tests for invariants
- [ ] Error cases tested
- [ ] Performance impact measured
- [ ] Documentation updated
- [ ] No decrease in coverage
- [ ] All tests pass locally
- [ ] CI pipeline green
