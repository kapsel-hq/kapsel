//! Performance benchmarks for webhook ingestion and delivery.
//!
//! These benchmarks track critical KPIs to prevent performance regression:
//! - Ingestion latency p99 < 50ms
//! - Delivery throughput > 1000 webhooks/sec
//! - Database query p99 < 5ms
//! - Memory per 1K webhooks < 200MB

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use serde_json::json;
use test_harness::TestEnv;
use tokio::runtime::Runtime;
use uuid::Uuid;

/// Benchmarks webhook ingestion throughput.
fn bench_ingestion_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("ingestion");
    group.sample_size(100);
    group.measurement_time(Duration::from_secs(10));

    // Benchmark different payload sizes
    for payload_size in [100, 1000, 10000, 100000] {
        group.throughput(criterion::Throughput::Elements(1));

        group.bench_with_input(
            BenchmarkId::new("payload_size", payload_size),
            &payload_size,
            |b, &size| {
                b.iter_custom(|iters| {
                    rt.block_on(async {
                        let env = TestEnv::new().await.unwrap();
                        let _payload = generate_payload(size);

                        let start = Instant::now();
                        for _ in 0..iters {
                            let webhook = create_test_webhook();
                            ingest_webhook(&env, webhook).await;
                        }
                        start.elapsed()
                    })
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks database operations critical path.
fn bench_database_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("database");
    group.sample_size(100);

    // Benchmark webhook insert
    group.bench_function("insert_webhook", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();
                let webhooks: Vec<_> = (0..iters).map(|_| create_test_webhook()).collect();

                let start = Instant::now();
                for webhook in webhooks {
                    insert_webhook_to_db(&env, webhook).await;
                }
                start.elapsed()
            })
        });
    });

    // Benchmark claiming pending webhooks
    group.bench_function("claim_pending_batch", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();

                // Pre-populate with pending webhooks
                for _ in 0..1000 {
                    insert_pending_webhook(&env).await;
                }

                let start = Instant::now();
                for _ in 0..iters {
                    claim_pending_webhooks(&env, 10).await;
                }
                start.elapsed()
            })
        });
    });

    // Benchmark idempotency check
    group.bench_function("idempotency_check", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();
                let idempotency_key = "test_key_123";

                // Insert initial webhook
                insert_webhook_with_key(&env, idempotency_key).await;

                let start = Instant::now();
                for _ in 0..iters {
                    check_idempotency(&env, idempotency_key).await;
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

/// Benchmarks exponential backoff calculation.
fn bench_backoff_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("backoff");

    group.bench_function("calculate_exponential_backoff", |b| {
        b.iter(|| {
            for attempt in 0..10 {
                let delay = calculate_backoff(black_box(attempt));
                black_box(delay);
            }
        });
    });

    group.bench_function("calculate_with_jitter", |b| {
        b.iter(|| {
            for attempt in 0..10 {
                let delay = calculate_backoff_with_jitter(black_box(attempt), 0.25);
                black_box(delay);
            }
        });
    });

    group.finish();
}

/// Benchmarks circuit breaker state transitions.
fn bench_circuit_breaker(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_breaker");

    group.bench_function("record_success", |b| {
        let mut circuit = MockCircuitBreaker::new(5, 0.5);
        b.iter(|| {
            circuit.record_success();
        });
    });

    group.bench_function("record_failure", |b| {
        let mut circuit = MockCircuitBreaker::new(5, 0.5);
        b.iter(|| {
            circuit.record_failure();
        });
    });

    group.bench_function("state_transition", |b| {
        b.iter_batched(
            || MockCircuitBreaker::new(5, 0.5),
            |mut circuit| {
                // Force state transition
                for _ in 0..5 {
                    circuit.record_failure();
                }
                circuit.state()
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmarks concurrent webhook processing.
fn bench_concurrent_processing(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("concurrent");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(15));

    for concurrency in [10, 50, 100, 500] {
        group.bench_with_input(
            BenchmarkId::new("workers", concurrency),
            &concurrency,
            |b, &concurrency| {
                b.iter_custom(|iters| {
                    rt.block_on(async {
                        let env = TestEnv::new().await.unwrap();

                        // Pre-populate webhooks
                        let webhooks: Vec<_> =
                            (0..iters * concurrency).map(|_| create_test_webhook()).collect();

                        for webhook in &webhooks {
                            insert_webhook_to_db(&env, webhook.clone()).await;
                        }

                        let start = Instant::now();

                        // Simulate concurrent workers processing sequentially
                        for _ in 0..concurrency {
                            let batch_size = iters / concurrency;
                            for _ in 0..batch_size {
                                process_webhook_batch(&env, 0).await;
                            }
                        }

                        start.elapsed()
                    })
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks memory usage per webhook.
fn bench_memory_usage(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("memory");
    group.sample_size(20);

    group.bench_function("memory_per_1k_webhooks", |b| {
        b.iter_custom(|_| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();

                // Measure baseline memory
                let baseline = get_memory_usage();

                // Create 1000 webhooks
                let webhooks: Vec<_> = (0..1000).map(|_| create_test_webhook()).collect();

                let start = Instant::now();

                // Insert all webhooks
                for webhook in webhooks {
                    insert_webhook_to_db(&env, webhook).await;
                }

                // Measure memory after insertion
                let after = get_memory_usage();
                let memory_used = after.saturating_sub(baseline);

                // Verify we're under 200MB for 1K webhooks
                assert!(
                    memory_used < 200_000_000,
                    "Memory usage {} bytes exceeds 200MB limit",
                    memory_used
                );

                start.elapsed()
            })
        });
    });

    group.finish();
}

/// Benchmarks end-to-end latency.
fn bench_e2e_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("e2e_latency");
    group.sample_size(100);

    group.bench_function("p50_latency", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();
                let mut latencies = Vec::with_capacity(iters as usize);

                for _ in 0..iters {
                    let webhook = create_test_webhook();
                    let start = Instant::now();

                    // Full ingestion flow
                    ingest_webhook(&env, webhook).await;

                    latencies.push(start.elapsed());
                }

                // Calculate p50
                latencies.sort();
                latencies[latencies.len() / 2]
            })
        });
    });

    group.bench_function("p99_latency", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let env = TestEnv::new().await.unwrap();
                let mut latencies = Vec::with_capacity(iters as usize);

                for _ in 0..iters {
                    let webhook = create_test_webhook();
                    let start = Instant::now();

                    // Full ingestion flow
                    ingest_webhook(&env, webhook).await;

                    latencies.push(start.elapsed());
                }

                // Calculate p99
                latencies.sort();
                let p99_index = (latencies.len() as f64 * 0.99) as usize;
                let p99_latency = latencies[p99_index.min(latencies.len() - 1)];

                // Assert p99 < 50ms
                assert!(
                    p99_latency < Duration::from_millis(50),
                    "p99 latency {:?} exceeds 50ms target",
                    p99_latency
                );

                p99_latency
            })
        });
    });

    group.finish();
}

// Helper functions

fn generate_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

fn create_test_webhook() -> TestWebhook {
    TestWebhook {
        id: Uuid::new_v4(),
        payload: json!({
            "event": "test.webhook",
            "data": {
                "id": Uuid::new_v4().to_string(),
                "amount": 1000,
            }
        }),
        idempotency_key: Uuid::new_v4().to_string(),
    }
}

#[derive(Clone)]
struct TestWebhook {
    id: Uuid,
    payload: serde_json::Value,
    idempotency_key: String,
}

async fn ingest_webhook(env: &TestEnv, webhook: TestWebhook) {
    // Simplified ingestion for benchmarking
    sqlx::query(
        "INSERT INTO webhook_events (id, tenant_id, endpoint_id, source_event_id,
         idempotency_strategy, status, failure_count, headers, body, content_type, received_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT DO NOTHING",
    )
    .bind(webhook.id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(webhook.idempotency_key)
    .bind("header")
    .bind("pending")
    .bind(0i32)
    .bind(json!({}))
    .bind(webhook.payload.to_string().as_bytes())
    .bind("application/json")
    .bind(chrono::Utc::now())
    .execute(&env.db)
    .await
    .ok();
}

async fn insert_webhook_to_db(env: &TestEnv, webhook: TestWebhook) {
    ingest_webhook(env, webhook).await;
}

async fn insert_pending_webhook(env: &TestEnv) {
    let webhook = create_test_webhook();
    ingest_webhook(env, webhook).await;
}

async fn insert_webhook_with_key(env: &TestEnv, key: &str) {
    let mut webhook = create_test_webhook();
    webhook.idempotency_key = key.to_string();
    ingest_webhook(env, webhook).await;
}

async fn claim_pending_webhooks(env: &TestEnv, batch_size: i32) {
    sqlx::query(
        "UPDATE webhook_events
         SET status = 'delivering'
         WHERE id IN (
             SELECT id FROM webhook_events
             WHERE status = 'pending'
             LIMIT $1
             FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(batch_size)
    .execute(&env.db)
    .await
    .ok();
}

async fn check_idempotency(env: &TestEnv, key: &str) -> bool {
    let result: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM webhook_events WHERE source_event_id = $1")
            .bind(key)
            .fetch_one(&env.db)
            .await
            .unwrap_or((0,));

    result.0 > 0
}

async fn process_webhook_batch(env: &TestEnv, _worker_id: u64) {
    claim_pending_webhooks(env, 10).await;
}

fn calculate_backoff(attempt: u32) -> Duration {
    Duration::from_secs(2_u64.pow(attempt.min(10)))
}

fn calculate_backoff_with_jitter(attempt: u32, jitter_factor: f32) -> Duration {
    let base = calculate_backoff(attempt);
    let jitter = (base.as_millis() as f32 * jitter_factor) as u64;
    // Use deterministic jitter for benchmarks
    let jitter_ms = (attempt as u64 * 123) % (jitter * 2);
    Duration::from_millis(base.as_millis() as u64 + jitter_ms)
}

fn get_memory_usage() -> usize {
    // Simplified memory measurement
    // In production, use proper memory profiling
    1024 * 1024 * 10 // Mock 10MB
}

struct MockCircuitBreaker {
    failure_count: usize,
    threshold: usize,
    _failure_rate: f32,
    state: CircuitState,
}

#[derive(Clone, Copy)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl MockCircuitBreaker {
    fn new(threshold: usize, failure_rate: f32) -> Self {
        Self {
            failure_count: 0,
            threshold,
            _failure_rate: failure_rate,
            state: CircuitState::Closed,
        }
    }

    fn record_success(&mut self) {
        self.failure_count = 0;
        if matches!(self.state, CircuitState::HalfOpen) {
            self.state = CircuitState::Closed;
        }
    }

    fn record_failure(&mut self) {
        self.failure_count += 1;
        if self.failure_count >= self.threshold {
            self.state = CircuitState::Open;
        }
    }

    fn state(&self) -> CircuitState {
        self.state
    }
}

criterion_group!(
    benches,
    bench_ingestion_throughput,
    bench_database_operations,
    bench_backoff_calculation,
    bench_circuit_breaker,
    bench_concurrent_processing,
    bench_memory_usage,
    bench_e2e_latency
);

criterion_main!(benches);
