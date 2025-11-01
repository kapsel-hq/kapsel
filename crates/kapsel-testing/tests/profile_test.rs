//! Profile test to measure database initialization performance.

use std::time::Instant;

use anyhow::Result;
use kapsel_testing::TestEnv;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn init_test_tracing() {
    let _ = tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("kapsel_testing=info"))
                .unwrap(),
        )
        .with(fmt::layer().with_target(true).with_thread_ids(true))
        .try_init();
}

#[tokio::test]
async fn profile_shared_database_initialization() -> Result<()> {
    init_test_tracing();

    let overall_start = Instant::now();
    println!("=== PROFILING: TestEnv::new_shared() ===");

    let env = TestEnv::new_shared().await?;

    let overall_time = overall_start.elapsed();
    println!("=== TOTAL TIME: {:?} ===", overall_time);

    // Verify the environment works
    let mut tx = env.pool().begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(&mut *tx).await?;
    println!("Tenant count: {}", count);

    Ok(())
}

#[tokio::test]
async fn profile_multiple_shared_initializations() -> Result<()> {
    init_test_tracing();

    println!("=== PROFILING: Multiple TestEnv::new_shared() calls ===");

    for i in 1..=5 {
        let start = Instant::now();
        let _env = TestEnv::new_shared().await?;
        let elapsed = start.elapsed();
        println!("Call {}: {:?}", i, elapsed);
    }

    Ok(())
}

#[tokio::test]
async fn profile_isolated_database_creation() -> Result<()> {
    init_test_tracing();

    let start = Instant::now();
    println!("=== PROFILING: TestEnv::new_isolated() ===");

    let env = TestEnv::new_isolated().await?;

    let elapsed = start.elapsed();
    println!("=== ISOLATED DB CREATION TIME: {:?} ===", elapsed);

    // Verify it works
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tenants").fetch_one(env.pool()).await?;
    println!("Tenant count in isolated DB: {}", count);

    Ok(())
}

#[tokio::test]
async fn profile_concurrent_initialization() -> Result<()> {
    init_test_tracing();

    println!("=== PROFILING: Concurrent TestEnv::new_shared() calls ===");

    let start = Instant::now();

    // Simulate multiple processes/tasks hitting initialization simultaneously
    let handles: Vec<_> = (0..10)
        .map(|i| {
            tokio::spawn(async move {
                let task_start = Instant::now();
                let _env = TestEnv::new_shared().await.unwrap();
                let elapsed = task_start.elapsed();
                println!("Task {}: {:?}", i, elapsed);
                elapsed
            })
        })
        .collect();

    let mut times = Vec::new();
    for handle in handles {
        times.push(handle.await.unwrap());
    }

    let total_time = start.elapsed();
    let avg_time = times.iter().sum::<std::time::Duration>() / times.len() as u32;
    let max_time = times.iter().max().unwrap();
    let min_time = times.iter().min().unwrap();

    println!("=== CONCURRENT STATS ===");
    println!("Total wall time: {:?}", total_time);
    println!("Average task time: {:?}", avg_time);
    println!("Min task time: {:?}", min_time);
    println!("Max task time: {:?}", max_time);
    println!(
        "Parallel efficiency: {:.1}%",
        (avg_time.as_secs_f64() / total_time.as_secs_f64()) * 100.0
    );

    Ok(())
}
