//! Configuration management for Kapsel webhook reliability service.

use std::{net::SocketAddr, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use kapsel_delivery::{
    circuit::CircuitConfig,
    client::ClientConfig,
    retry::{BackoffStrategy, RetryPolicy},
    worker::DeliveryConfig,
};
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "config.toml";

/// Complete service configuration with defaults, file, and environment
/// overrides.
///
/// Configuration is loaded in priority order:
/// 1. Environment variables (highest priority)
/// 2. Configuration file (`config.toml`)
/// 3. Built-in defaults (lowest priority)
///
/// The service works out-of-the-box with production-ready defaults.
/// Create `config.toml` to customize configuration for your environment.
/// Use environment variables for deployment-specific overrides.
///
/// # Example
///
/// ```no_run
/// use kapsel_api::Config;
///
/// // Load configuration from all sources
/// let config = Config::load().expect("Failed to load configuration");
///
/// println!("Server will bind to {}:{}", config.host, config.port);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // Database
    /// PostgreSQL connection URL.
    ///
    /// Environment variable: `DATABASE_URL`
    #[serde(default = "default_database_url", alias = "DATABASE_URL")]
    pub database_url: String,
    /// Maximum number of database connections in the pool.
    ///
    /// Environment variable: `DATABASE_MAX_CONNECTIONS`
    #[serde(default = "default_max_connections", alias = "DATABASE_MAX_CONNECTIONS")]
    pub database_max_connections: u32,
    /// Minimum number of connections to maintain in the pool.
    ///
    /// Environment variable: `DATABASE_MIN_CONNECTIONS`
    #[serde(default = "default_min_connections", alias = "DATABASE_MIN_CONNECTIONS")]
    pub database_min_connections: u32,
    /// Database connection acquire timeout in seconds.
    ///
    /// Environment variable: `DATABASE_CONNECTION_TIMEOUT`
    #[serde(default = "default_acquire_timeout", alias = "DATABASE_CONNECTION_TIMEOUT")]
    pub database_connection_timeout: u64,
    /// Database connection idle timeout in seconds.
    ///
    /// Environment variable: `DATABASE_IDLE_TIMEOUT`
    #[serde(default = "default_idle_timeout", alias = "DATABASE_IDLE_TIMEOUT")]
    pub database_idle_timeout: u64,
    /// Maximum lifetime of database connections in seconds.
    ///
    /// Environment variable: `DATABASE_MAX_LIFETIME`
    #[serde(default = "default_max_lifetime", alias = "DATABASE_MAX_LIFETIME")]
    pub database_max_lifetime: u64,

    // Server
    /// Server bind address.
    ///
    /// Environment variable: `HOST`
    #[serde(default = "default_host", alias = "HOST")]
    pub host: String,
    /// Server bind port.
    ///
    /// Environment variable: `PORT`
    #[serde(default = "default_port", alias = "PORT")]
    pub port: u16,
    /// HTTP request timeout in seconds.
    ///
    /// Environment variable: `REQUEST_TIMEOUT`
    #[serde(default = "default_request_timeout", alias = "REQUEST_TIMEOUT")]
    pub request_timeout: u64,

    // Delivery
    /// Number of concurrent delivery workers.
    ///
    /// Environment variable: `WORKER_POOL_SIZE`
    #[serde(default = "default_worker_count", alias = "WORKER_POOL_SIZE")]
    pub worker_pool_size: usize,
    /// Maximum events to claim per worker batch.
    ///
    /// Environment variable: `WORKER_QUEUE_SIZE`
    #[serde(default = "default_batch_size", alias = "WORKER_QUEUE_SIZE")]
    pub worker_queue_size: usize,

    // Retry
    /// Maximum retry attempts per webhook delivery.
    ///
    /// Environment variable: `MAX_RETRY_ATTEMPTS`
    #[serde(default = "default_retry_attempts", alias = "MAX_RETRY_ATTEMPTS")]
    pub max_retry_attempts: u32,
    /// Base delay for exponential backoff in milliseconds.
    ///
    /// Environment variable: `RETRY_BASE_DELAY_MS`
    #[serde(default = "default_base_delay_ms", alias = "RETRY_BASE_DELAY_MS")]
    pub retry_base_delay_ms: u64,
    /// Maximum delay between retries in milliseconds.
    ///
    /// Environment variable: `RETRY_MAX_DELAY_MS`
    #[serde(default = "default_max_delay_ms", alias = "RETRY_MAX_DELAY_MS")]
    pub retry_max_delay_ms: u64,
    /// Jitter factor for retry timing (0.0 to 1.0).
    ///
    /// Environment variable: `RETRY_JITTER_FACTOR`
    #[serde(default = "default_jitter_factor", alias = "RETRY_JITTER_FACTOR")]
    pub retry_jitter_factor: f64,

    // Circuit breaker
    /// Number of failures to trigger circuit breaker open state.
    ///
    /// Environment variable: `CIRCUIT_BREAKER_FAILURE_THRESHOLD`
    #[serde(default = "default_failure_threshold", alias = "CIRCUIT_BREAKER_FAILURE_THRESHOLD")]
    pub circuit_breaker_failure_threshold: u32,
    /// Number of successes to close circuit breaker from half-open.
    ///
    /// Environment variable: `CIRCUIT_BREAKER_SUCCESS_THRESHOLD`
    #[serde(default = "default_success_threshold", alias = "CIRCUIT_BREAKER_SUCCESS_THRESHOLD")]
    pub circuit_breaker_success_threshold: u32,
    /// Time in seconds to wait before transitioning from open to half-open.
    ///
    /// Environment variable: `CIRCUIT_BREAKER_TIMEOUT_SECONDS`
    #[serde(default = "default_circuit_timeout", alias = "CIRCUIT_BREAKER_TIMEOUT_SECONDS")]
    pub circuit_breaker_timeout_seconds: u64,

    // Client
    /// HTTP request timeout for webhook delivery in seconds.
    ///
    /// Environment variable: `DELIVERY_TIMEOUT_SECONDS`
    #[serde(default = "default_delivery_timeout", alias = "DELIVERY_TIMEOUT_SECONDS")]
    pub delivery_timeout_seconds: u64,

    // Attestation
    /// Batch size for processing attestation events.
    ///
    /// Environment variable: `ATTESTATION_BATCH_SIZE`
    #[serde(default = "default_attestation_batch_size", alias = "ATTESTATION_BATCH_SIZE")]
    pub attestation_batch_size: usize,

    // Logging
    /// Log level configuration.
    ///
    /// Environment variable: `RUST_LOG`
    #[serde(default = "default_log_level", alias = "RUST_LOG")]
    pub rust_log: String,
}

impl Config {
    /// Load configuration from defaults, config file, and environment variable
    /// overrides.
    ///
    /// Configuration priority (highest to lowest):
    /// 1. Environment variables (e.g., `DATABASE_URL`, `PORT`)
    /// 2. Configuration file (`config.toml`)
    /// 3. Built-in defaults (production-ready values)
    ///
    /// The system works out-of-the-box with sensible defaults. Create
    /// `config.toml` to customize configuration, or use environment
    /// variables for deployment-specific overrides.
    pub fn load() -> Result<Self> {
        let figment = Figment::new()
            .merge(Serialized::defaults(Self::default()))
            .merge(Toml::file(CONFIG_FILE))
            .merge(Env::prefixed(""));

        let config: Self = figment.extract().context("Failed to load configuration")?;
        config.validate()?;
        Ok(config)
    }

    /// Convert to the delivery crate's configuration types.
    pub fn to_delivery_config(&self) -> DeliveryConfig {
        DeliveryConfig {
            worker_count: self.worker_pool_size,
            batch_size: self.worker_queue_size,
            poll_interval: Duration::from_secs(1),
            client_config: self.to_client_config(),
            default_retry_policy: self.to_retry_policy(),
            shutdown_timeout: Duration::from_secs(self.delivery_timeout_seconds),
        }
    }

    /// Convert to client configuration.
    pub fn to_client_config(&self) -> ClientConfig {
        ClientConfig {
            timeout: Duration::from_secs(self.delivery_timeout_seconds),
            user_agent: "Kapsel/1.0".to_string(),
            max_redirects: 5,
            verify_tls: true,
        }
    }

    /// Convert to retry policy.
    pub fn to_retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: self.max_retry_attempts,
            base_delay: Duration::from_millis(self.retry_base_delay_ms),
            max_delay: Duration::from_millis(self.retry_max_delay_ms),
            jitter_factor: self.retry_jitter_factor,
            backoff_strategy: BackoffStrategy::Exponential,
        }
    }

    /// Converts to circuit breaker configuration with sensible hardcoded
    /// defaults.
    ///
    /// Some circuit breaker parameters are not exposed as configuration options
    /// to prevent misconfiguration that could degrade reliability:
    /// - `min_requests_for_rate`: 10 requests required before rate-based
    ///   tripping
    /// - `failure_rate_threshold`: 50% failure rate triggers circuit opening
    /// - `half_open_max_requests`: 2 test requests in half-open state
    pub fn to_circuit_config(&self) -> CircuitConfig {
        CircuitConfig {
            failure_threshold: self.circuit_breaker_failure_threshold,
            min_requests_for_rate: 10,
            failure_rate_threshold: 0.5,
            open_timeout: Duration::from_secs(self.circuit_breaker_timeout_seconds),
            success_threshold: self.circuit_breaker_success_threshold,
            half_open_max_requests: 2,
        }
    }

    /// Parse server socket address from host and port configuration.
    pub fn parse_server_addr(&self) -> Result<SocketAddr> {
        let addr_str = format!("{}:{}", self.host, self.port);
        SocketAddr::from_str(&addr_str).context("Invalid server address")
    }

    /// Get database URL with password masked for logging.
    pub fn database_url_masked(&self) -> String {
        if let Some(at_pos) = self.database_url.find('@') {
            if let Some(colon_pos) = self.database_url[..at_pos].rfind(':') {
                let mut masked = self.database_url.clone();
                masked.replace_range(colon_pos + 1..at_pos, "***");
                return masked;
            }
        }
        self.database_url.clone()
    }

    /// Validate configuration values.
    fn validate(&self) -> Result<()> {
        if self.port == 0 {
            anyhow::bail!("port must be greater than 0");
        }

        if self.database_max_connections == 0 {
            anyhow::bail!("database max_connections must be greater than 0");
        }

        if self.database_min_connections > self.database_max_connections {
            anyhow::bail!("database min_connections cannot exceed max_connections");
        }

        if self.worker_pool_size == 0 {
            anyhow::bail!("worker_pool_size must be greater than 0");
        }

        if self.worker_queue_size == 0 {
            anyhow::bail!("worker_queue_size must be greater than 0");
        }

        if self.max_retry_attempts == 0 {
            anyhow::bail!("max_retry_attempts must be greater than 0");
        }

        if !(0.0..=1.0).contains(&self.retry_jitter_factor) {
            anyhow::bail!("retry_jitter_factor must be between 0.0 and 1.0");
        }

        if self.circuit_breaker_failure_threshold == 0 {
            anyhow::bail!("circuit_breaker_failure_threshold must be greater than 0");
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: default_database_url(),
            database_max_connections: default_max_connections(),
            database_min_connections: default_min_connections(),
            database_connection_timeout: default_acquire_timeout(),
            database_idle_timeout: default_idle_timeout(),
            database_max_lifetime: default_max_lifetime(),
            host: default_host(),
            port: default_port(),
            request_timeout: default_request_timeout(),
            worker_pool_size: default_worker_count(),
            worker_queue_size: default_batch_size(),
            max_retry_attempts: default_retry_attempts(),
            retry_base_delay_ms: default_base_delay_ms(),
            retry_max_delay_ms: default_max_delay_ms(),
            retry_jitter_factor: default_jitter_factor(),
            circuit_breaker_failure_threshold: default_failure_threshold(),
            circuit_breaker_success_threshold: default_success_threshold(),
            circuit_breaker_timeout_seconds: default_circuit_timeout(),
            delivery_timeout_seconds: default_delivery_timeout(),
            attestation_batch_size: default_attestation_batch_size(),
            rust_log: default_log_level(),
        }
    }
}

fn default_database_url() -> String {
    "postgresql://localhost/kapsel".to_string()
}

fn default_max_connections() -> u32 {
    10
}

fn default_min_connections() -> u32 {
    2
}

fn default_acquire_timeout() -> u64 {
    10
}

fn default_idle_timeout() -> u64 {
    600
}

fn default_max_lifetime() -> u64 {
    1800
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_request_timeout() -> u64 {
    30
}

fn default_worker_count() -> usize {
    4
}

fn default_batch_size() -> usize {
    10
}

fn default_retry_attempts() -> u32 {
    5
}

fn default_base_delay_ms() -> u64 {
    1000
}

fn default_max_delay_ms() -> u64 {
    60000
}

fn default_jitter_factor() -> f64 {
    0.1
}

fn default_failure_threshold() -> u32 {
    5
}

fn default_success_threshold() -> u32 {
    2
}

fn default_circuit_timeout() -> u64 {
    30
}

fn default_delivery_timeout() -> u64 {
    30
}

fn default_attestation_batch_size() -> usize {
    100
}

fn default_log_level() -> String {
    "info".to_string()
}

#[cfg(test)]
#[allow(clippy::disallowed_types)]
#[allow(clippy::disallowed_methods)]
mod tests {
    //! Configuration loading tests using figment::Jail for isolation.
    //!
    //! These tests use `figment::Jail` to provide isolated environments for
    //! testing configuration loading with environment variables. Each test
    //! runs in its own sandboxed environment with isolated:
    //! - Environment variables (set with `jail.set_env()`)
    //! - File system (temporary directory with `jail.create_file()`)
    //! - Working directory
    //!
    //! This approach ensures tests can run in parallel without race conditions
    //! from environment variable contamination, replacing the previous approach
    //! that used a global mutex and manual environment variable cleanup.

    use figment::Jail;

    use super::*;

    #[test]
    fn default_config_snapshot() {
        let config = Config::default();

        assert!(config.validate().is_ok());

        // Snapshot key values to prevent regression
        insta::assert_yaml_snapshot!("default_config", serde_yaml::to_value(&config).unwrap());
    }

    /// Tests that environment variables properly override configuration
    /// defaults.
    ///
    /// This test verifies the environment variable override mechanism by:
    /// 1. Setting specific environment variables in an isolated jail
    /// 2. Loading configuration which should pick up these overrides
    /// 3. Snapshotting the resulting config to ensure values are correct
    ///
    /// Uses figment::Jail for parallel-safe environment isolation.
    #[test]
    fn config_with_env_overrides_snapshot() {
        Jail::expect_with(|jail| {
            jail.set_env("DATABASE_URL", "postgresql://env:override@localhost:5432/test_db");
            jail.set_env("DATABASE_MAX_CONNECTIONS", "25");
            jail.set_env("DATABASE_MIN_CONNECTIONS", "5");
            jail.set_env("DATABASE_CONNECTION_TIMEOUT", "15");
            jail.set_env("HOST", "127.0.0.1");
            jail.set_env("PORT", "9090");
            jail.set_env("REQUEST_TIMEOUT", "60");
            jail.set_env("WORKER_POOL_SIZE", "16");
            jail.set_env("WORKER_QUEUE_SIZE", "25");
            jail.set_env("MAX_RETRY_ATTEMPTS", "12");
            jail.set_env("RETRY_BASE_DELAY_MS", "2000");
            jail.set_env("RETRY_MAX_DELAY_MS", "120000");
            jail.set_env("RETRY_JITTER_FACTOR", "0.15");
            jail.set_env("CIRCUIT_BREAKER_FAILURE_THRESHOLD", "8");
            jail.set_env("CIRCUIT_BREAKER_SUCCESS_THRESHOLD", "3");
            jail.set_env("CIRCUIT_BREAKER_TIMEOUT_SECONDS", "60");
            jail.set_env("DELIVERY_TIMEOUT_SECONDS", "35");
            jail.set_env("ATTESTATION_BATCH_SIZE", "150");
            jail.set_env("RUST_LOG", "info,kapsel=debug");

            let config = Config::load().expect("Config should load with env overrides");

            assert!(config.validate().is_ok());

            insta::assert_yaml_snapshot!(
                "config_with_env_overrides",
                serde_yaml::to_value(&config).unwrap()
            );

            Ok(())
        });
    }

    /// Tests a production-like configuration with environment overrides.
    ///
    /// This test simulates a production deployment scenario where:
    /// 1. Environment variables provide production-specific values
    /// 2. Configuration loading correctly applies these overrides
    /// 3. The resulting config matches expected production settings
    ///
    /// Uses figment::Jail for parallel-safe environment isolation.
    #[test]
    fn production_like_config_snapshot() {
        Jail::expect_with(|jail| {
            jail.set_env("DATABASE_URL", "postgresql://prod:secret@db.example.com:5432/kapsel");
            jail.set_env("DATABASE_MAX_CONNECTIONS", "50");
            jail.set_env("DATABASE_MIN_CONNECTIONS", "10");
            jail.set_env("DATABASE_CONNECTION_TIMEOUT", "10");
            jail.set_env("HOST", "0.0.0.0");
            jail.set_env("PORT", "8080");
            jail.set_env("REQUEST_TIMEOUT", "30");
            jail.set_env("WORKER_POOL_SIZE", "32");
            jail.set_env("WORKER_QUEUE_SIZE", "50");
            jail.set_env("MAX_RETRY_ATTEMPTS", "15");
            jail.set_env("RETRY_BASE_DELAY_MS", "2000");
            jail.set_env("RETRY_MAX_DELAY_MS", "600000");
            jail.set_env("RETRY_JITTER_FACTOR", "0.1");
            jail.set_env("CIRCUIT_BREAKER_FAILURE_THRESHOLD", "8");
            jail.set_env("CIRCUIT_BREAKER_SUCCESS_THRESHOLD", "2");
            jail.set_env("CIRCUIT_BREAKER_TIMEOUT_SECONDS", "120");
            jail.set_env("DELIVERY_TIMEOUT_SECONDS", "45");
            jail.set_env("ATTESTATION_BATCH_SIZE", "500");
            jail.set_env("RUST_LOG", "info,kapsel=debug");

            let config = Config::load().expect("Config should load production settings");

            assert!(config.validate().is_ok());

            insta::assert_yaml_snapshot!(
                "production_like_config",
                serde_yaml::to_value(&config).unwrap()
            );

            Ok(())
        });
    }

    /// Tests configuration conversion methods with values from a config file.
    ///
    /// This test verifies that:
    /// 1. Configuration can be loaded from a TOML file in the jail directory
    /// 2. Config conversion methods (to_delivery_config, to_client_config,
    ///    etc.) work correctly with the loaded values
    /// 3. The converted configurations match expected snapshots
    ///
    /// Uses figment::Jail for isolated file system and config loading.
    #[test]
    fn config_conversions_snapshot() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                r"
                worker_pool_size = 8
                worker_queue_size = 25
                max_retry_attempts = 10
                retry_base_delay_ms = 2000
                retry_jitter_factor = 0.15
                retry_max_delay_ms = 300000
                circuit_breaker_failure_threshold = 10
                circuit_breaker_success_threshold = 3
                circuit_breaker_timeout_seconds = 60
                delivery_timeout_seconds = 45
                ",
            )?;

            let config = Config::load().expect("Config should load for conversion testing");

            let delivery_config = config.to_delivery_config();
            let client_config = config.to_client_config();
            let retry_policy = config.to_retry_policy();
            let circuit_config = config.to_circuit_config();

            let conversions = serde_json::json!({
                "circuit_config": {
                    "failure_threshold": circuit_config.failure_threshold,
                    "success_threshold": circuit_config.success_threshold,
                    "timeout_secs": circuit_config.open_timeout.as_secs(),
                },
                "client_config": {
                    "timeout_secs": client_config.timeout.as_secs(),
                    "user_agent": client_config.user_agent,
                },
                "delivery_config": {
                    "batch_size": delivery_config.batch_size,
                    "shutdown_timeout_secs": delivery_config.shutdown_timeout.as_secs(),
                    "worker_count": delivery_config.worker_count,
                },
                "retry_policy": {
                    "base_delay_ms": retry_policy.base_delay.as_millis(),
                    "jitter_factor": retry_policy.jitter_factor,
                    "max_attempts": retry_policy.max_attempts,
                    "max_delay_ms": retry_policy.max_delay.as_millis(),
                }
            });

            insta::assert_json_snapshot!("config_conversions", conversions);
            Ok(())
        });
    }

    #[test]
    fn invalid_config_validation_fails() {
        // Test invalid port
        let config = Config { port: 0, ..Default::default() };
        assert!(config.validate().is_err());

        // Reset and test invalid connection counts
        let config = Config { database_max_connections: 0, ..Default::default() };
        assert!(config.validate().is_err());

        let config = Config {
            database_min_connections: 100,
            database_max_connections: 10,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // Reset and test invalid worker count
        let config = Config { worker_pool_size: 0, ..Default::default() };
        assert!(config.validate().is_err());

        // Reset and test invalid batch size
        let config = Config { worker_queue_size: 0, ..Default::default() };
        assert!(config.validate().is_err());
    }

    /// Tests that database URL password masking works correctly.
    ///
    /// This test verifies that:
    /// 1. A database URL with credentials can be loaded from environment
    ///    variables
    /// 2. The masking function properly hides the password while preserving
    ///    other parts
    /// 3. Security-sensitive information is not exposed in logs
    ///
    /// Uses figment::Jail for isolated environment variable testing.
    #[test]
    fn database_url_masking() {
        Jail::expect_with(|jail| {
            jail.set_env(
                "DATABASE_URL",
                "postgresql://username:secret123@db.example.com:5432/kapsel",
            );

            let config = Config::load().expect("Config should load");
            let masked = config.database_url_masked();

            assert!(!masked.contains("secret123"));
            assert!(masked.contains("username"));
            assert!(masked.contains("db.example.com"));
            assert!(masked.contains("***"));

            Ok(())
        });
    }

    #[test]
    fn socket_address_parsing() {
        let config = Config { host: "127.0.0.1".to_string(), port: 9000, ..Default::default() };
        let addr = config.parse_server_addr().expect("Should parse socket address");

        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 9000);
    }
}
