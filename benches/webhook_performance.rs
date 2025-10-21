//! Performance benchmarks for Kapsel webhook reliability system.
//!
//! Focuses on computational performance without external dependencies.
//! Tracks critical performance metrics to prevent regressions.

use std::{collections::HashMap, hint::black_box, time::Duration};

use base64::prelude::*;
use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
// Imports for real circuit breaker benchmarks
use kapsel_delivery::circuit::{CircuitBreakerManager, CircuitConfig};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::runtime::Runtime;
use uuid::Uuid;

/// Benchmarks JSON parsing and serialization performance.
fn bench_json_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("json");
    group.sample_size(1000);

    // Test different payload sizes
    for payload_size in [100, 1000, 10000, 100000] {
        let payload = create_json_payload(payload_size);
        let json_string = payload.to_string();
        let json_bytes = json_string.as_bytes();

        group.throughput(Throughput::Bytes(json_bytes.len() as u64));

        group.bench_with_input(BenchmarkId::new("parse", payload_size), &json_bytes, |b, bytes| {
            b.iter(|| {
                let parsed: Value = serde_json::from_slice(black_box(bytes)).unwrap();
                black_box(parsed);
            });
        });

        group.bench_with_input(
            BenchmarkId::new("serialize", payload_size),
            &payload,
            |b, value| {
                b.iter(|| {
                    let serialized = serde_json::to_string(black_box(value)).unwrap();
                    black_box(serialized);
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks HMAC signature validation performance.
fn bench_signature_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("signatures");
    group.sample_size(1000);

    let secret = b"webhook_secret_key_for_benchmarking_performance";

    for payload_size in [100, 1000, 10000, 100000] {
        let payload = create_test_payload(payload_size);

        group.throughput(Throughput::Bytes(payload.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("hmac_sha256", payload_size),
            &payload,
            |b, bytes| {
                b.iter(|| {
                    let signature = compute_hmac_sha256(black_box(bytes), black_box(secret));
                    black_box(signature);
                });
            },
        );

        // Benchmark signature verification
        let signature = compute_hmac_sha256(&payload, secret);
        group.bench_with_input(
            BenchmarkId::new("verify_hmac", payload_size),
            &(payload, signature),
            |b, (input_data, sig)| {
                b.iter(|| {
                    let valid = verify_hmac_sha256(
                        black_box(input_data),
                        black_box(secret),
                        black_box(sig),
                    );
                    black_box(valid);
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks base64 encoding and decoding operations.
fn bench_base64_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("base64");
    group.sample_size(1000);

    for data_size in [100, 1000, 10000, 100000] {
        let payload = create_test_payload(data_size);
        let encoded = BASE64_STANDARD.encode(&payload);

        group.throughput(Throughput::Bytes(payload.len() as u64));

        group.bench_with_input(BenchmarkId::new("encode", data_size), &payload, |b, bytes| {
            b.iter(|| {
                let encoded = BASE64_STANDARD.encode(black_box(bytes));
                black_box(encoded);
            });
        });

        group.bench_with_input(
            BenchmarkId::new("decode", data_size),
            &encoded,
            |b, encoded_data| {
                b.iter(|| {
                    let decoded = BASE64_STANDARD.decode(black_box(encoded_data)).unwrap();
                    black_box(decoded);
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks exponential backoff calculation.
fn bench_backoff_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("backoff");
    group.sample_size(10000);

    group.bench_function("exponential_backoff", |b| {
        b.iter(|| {
            for attempt in 0..10 {
                let delay = calculate_exponential_backoff(black_box(attempt));
                black_box(delay);
            }
        });
    });

    group.bench_function("backoff_with_jitter", |b| {
        b.iter(|| {
            for attempt in 0..10 {
                let delay = calculate_backoff_with_jitter(black_box(attempt), 0.1);
                black_box(delay);
            }
        });
    });

    group.bench_function("bounded_backoff", |b| {
        b.iter(|| {
            for attempt in 0..20 {
                let delay = calculate_bounded_backoff(black_box(attempt), Duration::from_secs(300));
                black_box(delay);
            }
        });
    });

    group.finish();
}

/// Benchmarks UUID generation and string conversion.
fn bench_uuid_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("uuid");
    group.sample_size(10000);

    group.bench_function("generate_v4", |b| {
        b.iter(|| {
            let uuid = Uuid::new_v4();
            black_box(uuid);
        });
    });

    group.bench_function("to_string", |b| {
        let uuid = Uuid::new_v4();
        b.iter(|| {
            let string = uuid.to_string();
            black_box(string);
        });
    });

    group.bench_function("to_simple", |b| {
        let uuid = Uuid::new_v4();
        b.iter(|| {
            let simple = uuid.simple().to_string();
            black_box(simple);
        });
    });

    group.bench_function("from_string", |b| {
        let uuid_string = Uuid::new_v4().to_string();
        b.iter(|| {
            let parsed = Uuid::parse_str(black_box(&uuid_string)).unwrap();
            black_box(parsed);
        });
    });

    group.finish();
}

/// Benchmarks Bytes operations (zero-copy).
fn bench_bytes_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("bytes");
    group.sample_size(10000);

    for size in [1000, 10000, 100000, 1000000] {
        let payload = create_test_payload(size);
        let bytes = Bytes::from(payload);

        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("clone", size), &bytes, |b, bytes| {
            b.iter(|| {
                let cloned = bytes.clone();
                black_box(cloned);
            });
        });

        group.bench_with_input(BenchmarkId::new("slice", size), &bytes, |b, bytes| {
            b.iter(|| {
                let slice = bytes.slice(100..500);
                black_box(slice);
            });
        });

        let vec_data = bytes.to_vec();
        group.bench_with_input(BenchmarkId::new("from_vec", size), &vec_data, |b, vec| {
            b.iter(|| {
                let bytes = Bytes::from(black_box(vec.clone()));
                black_box(bytes);
            });
        });
    }

    group.finish();
}

/// Benchmarks circuit breaker state machine.
fn bench_circuit_breaker(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_breaker");
    group.sample_size(10000);

    // Fixed benchmark using iter_batched to prevent compiler optimization
    group.bench_function("record_success", |b| {
        b.iter_batched(
            || MockCircuitBreaker::new(10, 0.5),
            |mut breaker| {
                breaker.record_success();
                black_box(breaker);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Benchmark record_failure that causes state transition
    group.bench_function("record_failure_and_trip", |b| {
        b.iter_batched(
            || MockCircuitBreaker::new(1, 0.5), // Low threshold to trigger trip
            |mut breaker| {
                breaker.record_failure();
                black_box(breaker);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Benchmark transitioning from HalfOpen to Closed
    group.bench_function("halfopen_to_closed", |b| {
        b.iter_batched(
            || {
                let mut breaker = MockCircuitBreaker::new(1, 0.5);
                breaker.record_failure(); // Trip to Open
                breaker.state = CircuitState::HalfOpen; // Manually set to HalfOpen
                breaker
            },
            |mut breaker| {
                breaker.record_success(); // Should transition to Closed
                black_box(breaker);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.bench_function("is_open", |b| {
        b.iter_batched(
            || MockCircuitBreaker::new(10, 0.5),
            |breaker| {
                black_box(breaker.is_open());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmarks the real async CircuitBreakerManager from production code.
fn bench_real_circuit_breaker(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("real_circuit_breaker");
    group.sample_size(1000);

    let manager = CircuitBreakerManager::new(CircuitConfig::default());
    let endpoint_id = "test-endpoint";

    group.bench_function("record_success_async", |b| {
        // Tell Criterion to run this benchmark within the Tokio runtime
        b.to_async(&rt).iter(|| async {
            manager.record_success(black_box(endpoint_id)).await;
        });
    });

    group.bench_function("record_failure_async", |b| {
        b.to_async(&rt).iter(|| async {
            manager.record_failure(black_box(endpoint_id)).await;
        });
    });

    group.bench_function("should_allow_request_async", |b| {
        b.to_async(&rt).iter(|| async {
            let allowed = manager.should_allow_request(black_box(endpoint_id)).await;
            black_box(allowed);
        });
    });

    group.finish();
}

// TODO: Add high-level benchmarks for NFR validation
// These require additional TestEnv API development:
// - bench_ingestion_throughput: NFR-1.1, NFR-2.1
// - bench_database_operations: NFR-1.3 (Query P99 < 5ms)
// - bench_e2e_latency: NFR-1.1 (P99 < 50ms)

/// Benchmarks header parsing and manipulation.
fn bench_header_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("headers");
    group.sample_size(1000);

    let headers = create_test_headers();

    group.bench_function("parse_headers", |b| {
        b.iter(|| {
            let parsed = parse_headers(black_box(&headers));
            black_box(parsed);
        });
    });

    group.bench_function("serialize_headers", |b| {
        let header_map = parse_headers(&headers);
        b.iter(|| {
            let serialized = serialize_headers(black_box(&header_map));
            black_box(serialized);
        });
    });

    group.bench_function("extract_signature", |b| {
        b.iter(|| {
            let signature = extract_signature_header(black_box(&headers));
            black_box(signature);
        });
    });

    group.finish();
}

// Helper functions

fn create_json_payload(target_size: usize) -> Value {
    let base = json!({
        "event": "test.webhook",
        "id": Uuid::new_v4().to_string(),
        "timestamp": 1234567890u64,
        "data": {
            "object": "charge",
            "amount": 2000,
            "currency": "usd"
        }
    });

    let base_size = base.to_string().len();
    if target_size > base_size {
        let padding_size = target_size - base_size - 20; // Account for JSON overhead
        let padding = "x".repeat(padding_size);
        json!({
            "event": "test.webhook",
            "id": Uuid::new_v4().to_string(),
            "timestamp": 1234567890u64,
            "data": {
                "object": "charge",
                "amount": 2000,
                "currency": "usd"
            },
            "padding": padding
        })
    } else {
        base
    }
}

fn create_test_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 256) as u8).collect()
}

fn create_test_headers() -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), "Webhook-Sender/1.0".to_string()),
        ("X-Webhook-Signature".to_string(), "sha256=abcdef123456".to_string()),
        ("X-Webhook-ID".to_string(), Uuid::new_v4().to_string()),
        ("X-Webhook-Timestamp".to_string(), "1234567890".to_string()),
        ("Content-Length".to_string(), "1024".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        ("Authorization".to_string(), "Bearer token123".to_string()),
    ]
}

fn compute_hmac_sha256(bytes: &[u8], key: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(bytes);
    mac.finalize().into_bytes().to_vec()
}

fn verify_hmac_sha256(bytes: &[u8], key: &[u8], signature: &[u8]) -> bool {
    let expected = compute_hmac_sha256(bytes, key);
    expected.len() == signature.len() && expected.iter().zip(signature).all(|(a, b)| a == b)
}

fn calculate_exponential_backoff(attempt: u32) -> Duration {
    let base_delay = 1000; // 1 second in milliseconds
    let max_delay = 300_000; // 5 minutes in milliseconds

    let delay_ms = (base_delay * 2_u64.pow(attempt.min(10))).min(max_delay);
    Duration::from_millis(delay_ms)
}

fn calculate_backoff_with_jitter(attempt: u32, jitter_factor: f64) -> Duration {
    let base_delay = calculate_exponential_backoff(attempt);
    let jitter_range = (base_delay.as_millis() as f64 * jitter_factor) as u64;

    // Use attempt as seed for deterministic jitter
    let jitter = (attempt as u64 * 1103515245 + 12345) % (jitter_range * 2);
    let jitter_offset = jitter.saturating_sub(jitter_range);

    Duration::from_millis(base_delay.as_millis() as u64 + jitter_offset)
}

fn calculate_bounded_backoff(attempt: u32, max_delay: Duration) -> Duration {
    calculate_exponential_backoff(attempt).min(max_delay)
}

fn parse_headers(headers: &[(String, String)]) -> HashMap<String, String> {
    headers.iter().cloned().collect()
}

fn serialize_headers(headers: &HashMap<String, String>) -> String {
    headers.iter().map(|(k, v)| format!("{}: {}", k, v)).collect::<Vec<_>>().join("\n")
}

fn extract_signature_header(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.to_lowercase().contains("signature"))
        .map(|(_, value)| value.clone())
}

// Mock circuit breaker for benchmarking
struct MockCircuitBreaker {
    failure_count: u32,
    success_count: u32,
    threshold: u32,
    failure_rate: f64,
    pub state: CircuitState,
}

#[derive(Clone, Copy, Debug)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl MockCircuitBreaker {
    fn new(threshold: u32, failure_rate: f64) -> Self {
        Self {
            failure_count: 0,
            success_count: 0,
            threshold,
            failure_rate,
            state: CircuitState::Closed,
        }
    }

    fn record_success(&mut self) {
        self.success_count += 1;
        if matches!(self.state, CircuitState::HalfOpen) {
            self.state = CircuitState::Closed;
            self.failure_count = 0;
        }
    }

    fn record_failure(&mut self) {
        self.failure_count += 1;
        let total_requests = self.success_count + self.failure_count;

        if total_requests >= self.threshold {
            let current_failure_rate = self.failure_count as f64 / total_requests as f64;
            if current_failure_rate >= self.failure_rate {
                self.state = CircuitState::Open;
            }
        }
    }

    fn is_open(&self) -> bool {
        matches!(self.state, CircuitState::Open)
    }
}

criterion_group!(
    benches,
    bench_json_operations,
    bench_signature_validation,
    bench_base64_operations,
    bench_backoff_calculation,
    bench_uuid_operations,
    bench_bytes_operations,
    bench_circuit_breaker,
    bench_real_circuit_breaker,
    bench_header_operations,
);

criterion_main!(benches);
