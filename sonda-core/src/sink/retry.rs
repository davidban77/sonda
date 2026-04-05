//! Shared retry abstraction for network sinks.
//!
//! Provides [`RetryConfig`] (a serde-deserializable configuration) and
//! [`RetryPolicy`] (a resolved runtime struct with parsed [`Duration`] values).
//! The [`RetryPolicy::execute`] method runs a fallible operation in a loop with
//! exponential backoff and full jitter, classifying each error as retryable or
//! terminal via a caller-supplied closure.
//!
//! # Design
//!
//! - **Opt-in**: absence of a `retry:` block in YAML preserves current fire-and-forget
//!   behavior. Sinks check `Option<RetryPolicy>` and fall through to the one-shot path
//!   when `None`.
//! - **Zero per-event allocation**: the policy is constructed once at sink creation.
//!   The retry loop uses `std::thread::sleep` (sync-first architecture) and the
//!   existing [`splitmix64`](crate::util::splitmix64) function for jitter RNG.
//! - **Batch discard on exhaustion**: when all retries are spent the batch is
//!   discarded to prevent unbounded buffer growth. Synthetic data loss is acceptable.

use std::time::Duration;

use crate::config::validate::parse_duration;
use crate::{ConfigError, SondaError};

/// Serde-deserializable retry configuration embedded in sink YAML blocks.
///
/// All three fields are required when the `retry:` block is present.
///
/// # Example YAML
///
/// ```yaml
/// retry:
///   max_attempts: 3
///   initial_backoff: 100ms
///   max_backoff: 5s
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
pub struct RetryConfig {
    /// Number of retry attempts after the initial failure.
    ///
    /// The total number of calls is `max_attempts + 1` (one initial attempt
    /// plus up to `max_attempts` retries). Must be at least 1.
    pub max_attempts: u32,

    /// Duration string for the first retry delay (e.g. `"100ms"`, `"1s"`).
    ///
    /// Parsed by [`parse_duration`](crate::config::validate::parse_duration).
    pub initial_backoff: String,

    /// Duration string for the maximum backoff cap (e.g. `"5s"`, `"30s"`).
    ///
    /// Must be greater than or equal to `initial_backoff`. Parsed by
    /// [`parse_duration`](crate::config::validate::parse_duration).
    pub max_backoff: String,
}

/// Resolved retry policy with parsed [`Duration`] values.
///
/// Constructed once at sink creation via [`RetryPolicy::from_config`] and
/// stored as a field on each network sink. The [`execute`](RetryPolicy::execute)
/// method drives the retry loop.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (not counting the initial call).
    max_attempts: u32,
    /// Base delay for the first retry.
    initial_backoff: Duration,
    /// Upper bound on any single backoff sleep.
    max_backoff: Duration,
}

impl RetryPolicy {
    /// Build a [`RetryPolicy`] from a deserialized [`RetryConfig`], validating
    /// all fields.
    ///
    /// # Validation
    ///
    /// - `max_attempts` must be at least 1.
    /// - `initial_backoff` and `max_backoff` must be parseable duration strings.
    /// - `max_backoff` must be greater than or equal to `initial_backoff`.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] if any validation check fails.
    pub fn from_config(config: &RetryConfig) -> Result<Self, SondaError> {
        if config.max_attempts < 1 {
            return Err(SondaError::Config(ConfigError::invalid(
                "retry max_attempts must be at least 1",
            )));
        }

        let initial_backoff = parse_duration(&config.initial_backoff)?;
        let max_backoff = parse_duration(&config.max_backoff)?;

        if max_backoff < initial_backoff {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "retry max_backoff ({}) must be >= initial_backoff ({})",
                config.max_backoff, config.initial_backoff
            ))));
        }

        Ok(Self {
            max_attempts: config.max_attempts,
            initial_backoff,
            max_backoff,
        })
    }

    /// Execute `operation` with retry logic.
    ///
    /// Calls `operation()` once. On failure, calls `classify(&error)` to decide
    /// whether to retry. If the error is retryable and attempts remain, sleeps
    /// for a jittered exponential backoff and tries again.
    ///
    /// # Backoff formula
    ///
    /// ```text
    /// base = min(max_backoff, initial_backoff * 2^attempt)
    /// sleep = rand(0, base)   // full jitter
    /// ```
    ///
    /// # Arguments
    ///
    /// - `operation` — the fallible action to attempt. Called up to
    ///   `max_attempts + 1` times total.
    /// - `classify` — returns `true` if the error is transient and should be
    ///   retried, `false` if the error is permanent and should be returned
    ///   immediately.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if any attempt succeeds.
    /// - `Err(last_error)` if all attempts are exhausted or a non-retryable
    ///   error is encountered.
    pub fn execute<F, C>(&self, mut operation: F, classify: C) -> Result<(), SondaError>
    where
        F: FnMut() -> Result<(), SondaError>,
        C: Fn(&SondaError) -> bool,
    {
        let mut last_error = match operation() {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };

        for attempt in 0..self.max_attempts {
            if !classify(&last_error) {
                return Err(last_error);
            }

            let backoff = self.jittered_backoff(attempt);
            eprintln!(
                "sonda: retry {}/{} after {}ms (error: {})",
                attempt + 1,
                self.max_attempts,
                backoff.as_millis(),
                last_error,
            );

            std::thread::sleep(backoff);

            match operation() {
                Ok(()) => return Ok(()),
                Err(e) => last_error = e,
            }
        }

        eprintln!(
            "sonda: all {} retries exhausted (last error: {})",
            self.max_attempts, last_error,
        );

        Err(last_error)
    }

    /// Compute a jittered backoff duration for the given attempt index (0-based).
    ///
    /// Uses exponential backoff with full jitter:
    /// `sleep = rand(0, min(max_backoff, initial_backoff * 2^attempt))`
    ///
    /// The jitter RNG is seeded from the backoff nanos and the current thread ID
    /// to avoid synchronized retries across sinks.
    fn jittered_backoff(&self, attempt: u32) -> Duration {
        // Compute the exponential base: initial_backoff * 2^attempt, capped at
        // max_backoff. Use checked_shl to avoid overflow for large attempt values.
        let multiplier: u32 = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        let base = self.initial_backoff.saturating_mul(multiplier);
        let capped = base.min(self.max_backoff);

        // Full jitter: uniform random in [0, capped].
        let nanos = capped.as_nanos() as u64;
        if nanos == 0 {
            return Duration::ZERO;
        }

        // Seed from attempt + a hash of the thread name to decorrelate across
        // sinks without relying on the unstable ThreadId::as_u64().
        let thread_hash = {
            let name = std::thread::current().name().unwrap_or("").to_owned();
            let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
            for byte in name.bytes() {
                h ^= byte as u64;
                h = h.wrapping_mul(0x0100_0000_01b3); // FNV-1a prime
            }
            h
        };
        let seed = (attempt as u64)
            .wrapping_mul(0x517c_c1b7_2722_0a95)
            .wrapping_add(thread_hash);
        let hash = crate::util::splitmix64(seed);
        let jittered_nanos = hash % (nanos + 1);

        Duration::from_nanos(jittered_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RetryConfig construction and validation ----

    #[test]
    fn from_config_with_valid_values_succeeds() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: "100ms".to_string(),
            max_backoff: "5s".to_string(),
        };
        let policy = RetryPolicy::from_config(&config).expect("should succeed");
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.initial_backoff, Duration::from_millis(100));
        assert_eq!(policy.max_backoff, Duration::from_secs(5));
    }

    #[test]
    fn from_config_with_equal_backoffs_succeeds() {
        let config = RetryConfig {
            max_attempts: 1,
            initial_backoff: "1s".to_string(),
            max_backoff: "1s".to_string(),
        };
        let policy = RetryPolicy::from_config(&config).expect("should succeed");
        assert_eq!(policy.initial_backoff, policy.max_backoff);
    }

    #[test]
    fn from_config_zero_attempts_returns_error() {
        let config = RetryConfig {
            max_attempts: 0,
            initial_backoff: "100ms".to_string(),
            max_backoff: "5s".to_string(),
        };
        let err = RetryPolicy::from_config(&config).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_attempts") && msg.contains("at least 1"),
            "expected validation message about max_attempts, got: {msg}"
        );
    }

    #[test]
    fn from_config_max_less_than_initial_returns_error() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: "5s".to_string(),
            max_backoff: "100ms".to_string(),
        };
        let err = RetryPolicy::from_config(&config).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_backoff") && msg.contains("initial_backoff"),
            "expected message about max_backoff >= initial_backoff, got: {msg}"
        );
    }

    #[test]
    fn from_config_invalid_initial_backoff_returns_error() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: "not-a-duration".to_string(),
            max_backoff: "5s".to_string(),
        };
        assert!(RetryPolicy::from_config(&config).is_err());
    }

    #[test]
    fn from_config_invalid_max_backoff_returns_error() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: "100ms".to_string(),
            max_backoff: "bad".to_string(),
        };
        assert!(RetryPolicy::from_config(&config).is_err());
    }

    // ---- Serde round-trip ----

    #[cfg(feature = "config")]
    #[test]
    fn retry_config_deserializes_from_yaml() {
        let yaml = r#"
max_attempts: 5
initial_backoff: 200ms
max_backoff: 10s
"#;
        let config: RetryConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        assert_eq!(config.max_attempts, 5);
        assert_eq!(config.initial_backoff, "200ms");
        assert_eq!(config.max_backoff, "10s");
    }

    #[cfg(feature = "config")]
    #[test]
    fn retry_config_round_trip_through_policy() {
        let yaml = r#"
max_attempts: 3
initial_backoff: 100ms
max_backoff: 5s
"#;
        let config: RetryConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        let policy = RetryPolicy::from_config(&config).expect("should validate");
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.initial_backoff, Duration::from_millis(100));
        assert_eq!(policy.max_backoff, Duration::from_secs(5));
    }

    // ---- Backoff behavior ----

    #[test]
    fn jittered_backoff_is_at_most_initial_for_attempt_zero() {
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(10),
        };
        // Run many times to check the upper bound probabilistically.
        for _ in 0..100 {
            let backoff = policy.jittered_backoff(0);
            assert!(
                backoff <= Duration::from_millis(100),
                "attempt 0 backoff {} must be <= 100ms",
                backoff.as_millis()
            );
        }
    }

    #[test]
    fn jittered_backoff_capped_at_max_backoff() {
        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
        };
        // At attempt 10, uncapped exponential would be 100ms * 2^10 = 102400ms.
        // Should be capped at 500ms.
        for _ in 0..100 {
            let backoff = policy.jittered_backoff(10);
            assert!(
                backoff <= Duration::from_millis(500),
                "backoff {} must be <= 500ms max_backoff",
                backoff.as_millis()
            );
        }
    }

    #[test]
    fn jittered_backoff_with_zero_duration_returns_zero() {
        let policy = RetryPolicy {
            max_attempts: 1,
            // parse_duration rejects zero, but we can construct directly for testing.
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        };
        let backoff = policy.jittered_backoff(0);
        assert_eq!(backoff, Duration::ZERO);
    }

    // ---- execute: success on first attempt ----

    #[test]
    fn execute_succeeds_on_first_attempt() {
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let mut calls = 0u32;
        let result = policy.execute(
            || {
                calls += 1;
                Ok(())
            },
            |_| true,
        );
        assert!(result.is_ok());
        assert_eq!(calls, 1, "should only call once on immediate success");
    }

    // ---- execute: retries on transient error then succeeds ----

    #[test]
    fn execute_retries_transient_error_then_succeeds() {
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let mut calls = 0u32;
        let result = policy.execute(
            || {
                calls += 1;
                if calls < 3 {
                    Err(SondaError::Sink(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "transient",
                    )))
                } else {
                    Ok(())
                }
            },
            |_| true,
        );
        assert!(result.is_ok());
        assert_eq!(calls, 3, "should call 1 initial + 2 retries");
    }

    // ---- execute: exhausts all retries ----

    #[test]
    fn execute_exhausts_retries_returns_last_error() {
        let policy = RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let mut calls = 0u32;
        let result = policy.execute(
            || {
                calls += 1;
                Err(SondaError::Sink(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "always fails",
                )))
            },
            |_| true,
        );
        assert!(result.is_err());
        assert_eq!(calls, 3, "should call 1 initial + 2 retries");
    }

    // ---- execute: non-retryable error returns immediately ----

    #[test]
    fn execute_non_retryable_error_returns_immediately() {
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let mut calls = 0u32;
        let result = policy.execute(
            || {
                calls += 1;
                Err(SondaError::Sink(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "permanent 4xx",
                )))
            },
            |_| false, // nothing is retryable
        );
        assert!(result.is_err());
        assert_eq!(calls, 1, "non-retryable error should not trigger retries");
    }

    // ---- execute: classifier selectively retries ----

    #[test]
    fn execute_classifier_distinguishes_retryable_from_permanent() {
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let mut calls = 0u32;
        let result = policy.execute(
            || {
                calls += 1;
                if calls == 1 {
                    // First failure: retryable (connection reset)
                    Err(SondaError::Sink(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "transient",
                    )))
                } else {
                    // Second failure: permanent (invalid input)
                    Err(SondaError::Sink(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "permanent",
                    )))
                }
            },
            |err| {
                // Only retry connection reset errors.
                matches!(err, SondaError::Sink(ref io_err) if io_err.kind() == std::io::ErrorKind::ConnectionReset)
            },
        );
        assert!(result.is_err());
        assert_eq!(
            calls, 2,
            "should retry once (transient) then stop (permanent)"
        );
    }

    // ---- Contract: Send + Sync ----

    #[test]
    fn retry_policy_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RetryPolicy>();
    }

    #[test]
    fn retry_config_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RetryConfig>();
    }

    // ---- Debug formatting ----

    #[test]
    fn retry_policy_is_debuggable() {
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        };
        let s = format!("{policy:?}");
        assert!(s.contains("RetryPolicy"));
    }

    #[test]
    fn retry_config_is_cloneable() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: "100ms".to_string(),
            max_backoff: "5s".to_string(),
        };
        let cloned = config.clone();
        assert_eq!(cloned.max_attempts, 3);
        assert_eq!(cloned.initial_backoff, "100ms");
        assert_eq!(cloned.max_backoff, "5s");
    }
}
