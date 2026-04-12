//! sonda-core — the engine for synthetic telemetry generation.
//!
//! This crate owns all domain logic: telemetry models, value generators,
//! schedulers, encoders, and sinks. The CLI and HTTP server are thin layers
//! that call into this library.

pub mod compiler;
pub mod config;
pub mod encoder;
pub mod generator;
pub mod model;
pub mod packs;
pub mod scenarios;
pub mod schedule;
pub mod sink;
pub(crate) mod util;

pub use config::aliases::{desugar_entry, desugar_scenario_config};
pub use config::BaseScheduleConfig;
pub use config::BurstConfig;
pub use config::CardinalitySpikeConfig;
pub use config::DistributionConfig;
pub use config::DynamicLabelConfig;
pub use config::DynamicLabelStrategy;
pub use config::HistogramScenarioConfig;
pub use config::LogScenarioConfig;
pub use config::MultiScenarioConfig;
pub use config::ScenarioEntry;
pub use config::SpikeStrategy;
pub use config::SummaryScenarioConfig;
pub use config::{expand_entry, expand_scenario};
pub use model::log::LogEvent;
pub use model::log::Severity;
pub use model::metric::Labels;
pub use model::metric::MetricEvent;
pub use model::metric::ValidatedMetricName;
pub use scenarios::BuiltinScenario;
pub use schedule::handle::ScenarioHandle;
pub use schedule::launch::{launch_scenario, prepare_entries, validate_entry, PreparedEntry};
pub use schedule::stats::ScenarioStats;

/// Top-level error type for sonda-core.
///
/// Each variant delegates to a typed sub-enum that preserves the original
/// error source where possible. This enables callers to programmatically
/// inspect error origins (e.g., distinguish `io::ErrorKind::NotFound` from
/// `PermissionDenied` in a generator file-read error) via the standard
/// [`std::error::Error::source`] chain.
///
/// The `Sink` variant wraps [`std::io::Error`] without a blanket `#[from]`
/// conversion — all I/O errors must be explicitly mapped to the correct
/// variant at the call site. This prevents generator or config I/O errors
/// from being misclassified as sink errors.
#[derive(Debug, thiserror::Error)]
pub enum SondaError {
    /// An error in scenario configuration (invalid values, missing fields).
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    /// An error during event encoding (serialization, timestamp, protobuf).
    #[error("encoder error: {0}")]
    Encoder(#[from] EncoderError),

    /// An I/O error originating from a sink (stdout, file, TCP, UDP, HTTP).
    ///
    /// This variant does **not** use `#[from] std::io::Error` because not all
    /// I/O errors originate from sinks. Generator file reads, for example,
    /// produce [`SondaError::Generator`] instead.
    #[error("sink error: {0}")]
    Sink(std::io::Error),

    /// An error from a generator (file I/O, invalid data).
    #[error("generator error: {0}")]
    Generator(#[from] GeneratorError),

    /// A runtime or system error (thread spawn failure, thread panic).
    ///
    /// These are environmental failures that are outside the user's control
    /// and cannot be fixed by editing configuration. Separated from
    /// [`ConfigError`] so that consumers matching on config errors to
    /// surface YAML validation feedback are not confused by thread panics.
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
}

/// Errors related to scenario configuration validation.
///
/// Covers invalid field values, missing required fields, unparseable
/// durations, and similar problems that the user can fix by editing their
/// YAML scenario file or adjusting programmatic config construction.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A configuration field has an invalid value.
    ///
    /// The message includes the field name and a human-readable explanation
    /// of the constraint that was violated.
    #[error("{0}")]
    InvalidValue(String),
}

impl ConfigError {
    /// Create a new [`ConfigError::InvalidValue`] from any displayable message.
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        ConfigError::InvalidValue(msg.into())
    }
}

/// Errors originating from value or log generators.
///
/// Currently contains [`FileRead`](GeneratorError::FileRead) for I/O failures
/// when loading generator data from disk. This enum is designed for
/// extensibility — future variants may include `InvalidData` (malformed file
/// contents), `ParseFailed` (unparseable numeric columns), or
/// `UnsupportedFormat` as generator capabilities grow.
#[derive(Debug, thiserror::Error)]
pub enum GeneratorError {
    /// Failed to read a generator input file (CSV replay, log replay).
    ///
    /// Preserves the original [`std::io::Error`] via the `#[source]` attribute
    /// so callers can inspect the error kind (e.g., `ErrorKind::NotFound` vs
    /// `ErrorKind::PermissionDenied`) programmatically.
    #[error("cannot read file {path:?}")]
    FileRead {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl GeneratorError {
    /// Returns the [`std::io::ErrorKind`] of the underlying I/O error, if this
    /// is a [`FileRead`](GeneratorError::FileRead) variant.
    ///
    /// Convenience method that lets callers inspect the error kind without
    /// manually traversing the `source()` chain.
    pub fn source_io_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            GeneratorError::FileRead { source, .. } => Some(source.kind()),
        }
    }
}

/// Errors during event encoding (serialization, timestamp conversion, protobuf).
///
/// Preserves original error sources where possible so callers can inspect
/// the underlying failure without string parsing.
#[derive(Debug, thiserror::Error)]
pub enum EncoderError {
    /// JSON serialization failed.
    ///
    /// Preserves the original [`serde_json::Error`] so callers can inspect
    /// whether the failure was due to I/O, data, syntax, or EOF conditions.
    #[error("JSON serialization failed")]
    SerializationFailed(#[source] serde_json::Error),

    /// The event timestamp predates the Unix epoch.
    ///
    /// Preserves the original [`std::time::SystemTimeError`] so callers can
    /// inspect how far before the epoch the timestamp was.
    #[error("timestamp before Unix epoch")]
    TimestampBeforeEpoch(#[source] std::time::SystemTimeError),

    /// The encoder does not support this event type (e.g., a metric-only
    /// encoder receiving a log event).
    #[error("{0}")]
    NotSupported(String),

    /// A catch-all for encoder errors that do not fit other variants.
    ///
    /// Used for feature-gated encoders (protobuf, snappy) where preserving
    /// the concrete error type would require conditional compilation on the
    /// enum definition itself.
    #[error("{0}")]
    Other(String),
}

/// Runtime and system errors outside the user's control.
///
/// These represent environmental failures (OS thread limits, thread panics)
/// that cannot be resolved by changing configuration.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The OS refused to spawn a new thread.
    ///
    /// Preserves the original [`std::io::Error`] via the `#[source]` attribute
    /// so callers can inspect the error kind (e.g., resource exhaustion)
    /// programmatically via the standard [`std::error::Error::source`] chain.
    #[error("failed to spawn scenario thread")]
    SpawnFailed(#[source] std::io::Error),

    /// A scenario thread panicked during execution.
    #[error("scenario thread panicked")]
    ThreadPanicked,

    /// One or more scenarios in a multi-scenario run failed.
    ///
    /// The error messages from all failed scenario threads are collected and
    /// joined into a single string. This variant exists to prevent thread-level
    /// sink, runtime, or generator errors from being misclassified as
    /// [`ConfigError`] when collected at the multi-runner level.
    #[error("{0}")]
    ScenariosFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SondaError variant discrimination ------------------------------------

    #[test]
    fn io_error_does_not_auto_convert_to_sonda_error() {
        // Verify that there is no From<io::Error> for SondaError.
        // SondaError::Sink must be constructed explicitly.
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let sonda_err = SondaError::Sink(io_err);
        assert!(
            matches!(sonda_err, SondaError::Sink(_)),
            "explicit Sink construction must produce Sink variant"
        );
    }

    #[test]
    fn missing_replay_file_produces_generator_error_not_sink() {
        let path = std::path::Path::new("/nonexistent/path/for/replay.log");
        let result = generator::log_replay::LogReplayGenerator::from_file(path);
        match result {
            Err(ref err) => {
                assert!(
                    matches!(err, SondaError::Generator(_)),
                    "missing replay file must produce Generator variant, got: {err:?}"
                );
            }
            Ok(_) => panic!("missing file must return Err"),
        }
    }

    #[test]
    fn missing_csv_file_produces_generator_error_not_sink() {
        let result = generator::csv_replay::CsvReplayGenerator::new(
            "/nonexistent/path/for/data.csv",
            0,
            true,
        );
        match result {
            Err(SondaError::Generator(GeneratorError::FileRead {
                ref path,
                ref source,
            })) => {
                assert_eq!(path, "/nonexistent/path/for/data.csv");
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            Err(ref err) => {
                panic!("missing CSV file must produce Generator(FileRead) variant, got: {err:?}");
            }
            Ok(_) => panic!("missing CSV file must return Err"),
        }
    }

    #[test]
    fn log_replay_factory_missing_file_produces_generator_error() {
        let config = generator::LogGeneratorConfig::Replay {
            file: "/nonexistent/path/for/replay.log".to_string(),
        };
        let result = generator::create_log_generator(&config);
        match result {
            Err(ref err) => {
                assert!(
                    matches!(err, SondaError::Generator(_)),
                    "factory with missing replay file must produce Generator variant, got: {err:?}"
                );
            }
            Ok(_) => panic!("missing replay file must return Err"),
        }
    }

    #[test]
    fn sink_file_error_produces_sink_variant() {
        // Opening a file at an invalid path must produce SondaError::Sink.
        let result = sink::file::FileSink::new(std::path::Path::new(
            "/nonexistent/deeply/nested/path/output.txt",
        ));
        match result {
            Err(ref err) => {
                assert!(
                    matches!(err, SondaError::Sink(_)),
                    "file sink I/O error must produce Sink variant, got: {err:?}"
                );
            }
            Ok(_) => panic!("invalid file path must return Err"),
        }
    }

    #[test]
    fn sonda_error_display_includes_context() {
        let err = SondaError::Generator(GeneratorError::FileRead {
            path: "/some/file".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        });
        let msg = format!("{err}");
        assert!(
            msg.contains("generator error"),
            "Generator variant display must include 'generator error', got: {msg}"
        );
        assert!(
            msg.contains("/some/file"),
            "Generator variant display must include the file path, got: {msg}"
        );
    }

    // ---- Sub-enum From conversions ------------------------------------------

    #[test]
    fn config_error_converts_to_sonda_error_via_from() {
        let config_err = ConfigError::invalid("rate must be positive");
        let sonda_err: SondaError = config_err.into();
        assert!(
            matches!(sonda_err, SondaError::Config(_)),
            "ConfigError must convert to SondaError::Config"
        );
    }

    #[test]
    fn generator_error_converts_to_sonda_error_via_from() {
        let gen_err = GeneratorError::FileRead {
            path: "/tmp/test.csv".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        let sonda_err: SondaError = gen_err.into();
        assert!(
            matches!(sonda_err, SondaError::Generator(_)),
            "GeneratorError must convert to SondaError::Generator"
        );
    }

    #[test]
    fn encoder_error_converts_to_sonda_error_via_from() {
        let enc_err = EncoderError::NotSupported("log encoding not supported".into());
        let sonda_err: SondaError = enc_err.into();
        assert!(
            matches!(sonda_err, SondaError::Encoder(_)),
            "EncoderError must convert to SondaError::Encoder"
        );
    }

    #[test]
    fn runtime_error_converts_to_sonda_error_via_from() {
        let rt_err = RuntimeError::ThreadPanicked;
        let sonda_err: SondaError = rt_err.into();
        assert!(
            matches!(sonda_err, SondaError::Runtime(_)),
            "RuntimeError must convert to SondaError::Runtime"
        );
    }

    // ---- source() chain preservation ----------------------------------------

    #[test]
    fn generator_file_read_preserves_io_error_source() {
        use std::error::Error;

        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let gen_err = GeneratorError::FileRead {
            path: "/secret/file".to_string(),
            source: io_err,
        };

        // The source() chain must be present and be an io::Error.
        let source = gen_err.source().expect("source() must return Some");
        let io_source = source
            .downcast_ref::<std::io::Error>()
            .expect("source must be std::io::Error");
        assert_eq!(io_source.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn generator_file_read_io_error_kind_is_inspectable() {
        let gen_err = GeneratorError::FileRead {
            path: "/missing/file".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        // Callers can match on the io::Error kind programmatically.
        assert_eq!(gen_err.source_io_kind(), Some(std::io::ErrorKind::NotFound));
    }

    #[test]
    fn encoder_serialization_preserves_serde_json_source() {
        use std::error::Error;

        // Provoke a real serde_json error by deserializing invalid JSON.
        let json_err: serde_json::Error = serde_json::from_str::<serde_json::Value>("{{invalid}}")
            .expect_err("invalid JSON must fail");
        let enc_err = EncoderError::SerializationFailed(json_err);

        let source = enc_err.source().expect("source() must return Some");
        assert!(
            source.downcast_ref::<serde_json::Error>().is_some(),
            "source must be serde_json::Error"
        );
    }

    #[test]
    fn encoder_timestamp_preserves_system_time_source() {
        use std::error::Error;

        let pre_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(1);
        let sys_err = pre_epoch.duration_since(std::time::UNIX_EPOCH).unwrap_err();
        let enc_err = EncoderError::TimestampBeforeEpoch(sys_err);

        let source = enc_err.source().expect("source() must return Some");
        assert!(
            source
                .downcast_ref::<std::time::SystemTimeError>()
                .is_some(),
            "source must be SystemTimeError"
        );
    }

    // ---- Runtime error classification (WARNING 1) ---------------------------

    #[test]
    fn spawn_failed_is_runtime_not_config() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "resource limit");
        let rt_err = RuntimeError::SpawnFailed(io_err);
        let sonda_err: SondaError = rt_err.into();
        assert!(
            matches!(sonda_err, SondaError::Runtime(RuntimeError::SpawnFailed(_))),
            "thread spawn failure must be Runtime::SpawnFailed, not Config"
        );
    }

    #[test]
    fn thread_panicked_is_runtime_not_config() {
        let rt_err = RuntimeError::ThreadPanicked;
        let sonda_err: SondaError = rt_err.into();
        assert!(
            matches!(sonda_err, SondaError::Runtime(RuntimeError::ThreadPanicked)),
            "thread panic must be Runtime::ThreadPanicked, not Config"
        );
    }

    #[test]
    fn runtime_error_display_is_descriptive() {
        let spawn_err = RuntimeError::SpawnFailed(std::io::Error::new(
            std::io::ErrorKind::Other,
            "too many threads",
        ));
        let msg = format!("{spawn_err}");
        assert!(
            msg.contains("failed to spawn scenario thread"),
            "SpawnFailed display must describe the spawn failure, got: {msg}"
        );

        let panic_err = RuntimeError::ThreadPanicked;
        let msg = format!("{panic_err}");
        assert!(
            msg.contains("panicked"),
            "ThreadPanicked display must mention panic, got: {msg}"
        );

        let scenarios_err =
            RuntimeError::ScenariosFailed("sink error: broken pipe; sink error: timeout".into());
        let msg = format!("{scenarios_err}");
        assert!(
            msg.contains("sink error"),
            "ScenariosFailed display must include the collected messages, got: {msg}"
        );
    }

    #[test]
    fn spawn_failed_preserves_io_error_source() {
        use std::error::Error;

        let io_err = std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "resource temporarily unavailable",
        );
        let rt_err = RuntimeError::SpawnFailed(io_err);

        let source = rt_err
            .source()
            .expect("SpawnFailed source() must return Some");
        let io_source = source
            .downcast_ref::<std::io::Error>()
            .expect("source must be std::io::Error");
        assert_eq!(io_source.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn spawn_failed_source_chain_traverses_through_sonda_error() {
        use std::error::Error;

        let io_err =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "cannot create thread");
        let sonda_err = SondaError::Runtime(RuntimeError::SpawnFailed(io_err));

        // SondaError::Runtime.source() -> RuntimeError
        let runtime_source = sonda_err
            .source()
            .expect("SondaError::Runtime source() must return Some");
        let rt_err = runtime_source
            .downcast_ref::<RuntimeError>()
            .expect("first source must be RuntimeError");

        // RuntimeError::SpawnFailed.source() -> io::Error
        let io_source = rt_err
            .source()
            .expect("SpawnFailed source() must return Some");
        let io_inner = io_source
            .downcast_ref::<std::io::Error>()
            .expect("second source must be std::io::Error");
        assert_eq!(io_inner.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn scenarios_failed_is_runtime_not_config() {
        let rt_err = RuntimeError::ScenariosFailed("thread failed".into());
        let sonda_err: SondaError = rt_err.into();
        assert!(
            matches!(
                sonda_err,
                SondaError::Runtime(RuntimeError::ScenariosFailed(_))
            ),
            "multi-scenario failures must be Runtime::ScenariosFailed, not Config"
        );
    }

    #[test]
    fn scenarios_failed_converts_to_sonda_error_via_from() {
        let rt_err = RuntimeError::ScenariosFailed("sink error: broken pipe".into());
        let sonda_err: SondaError = rt_err.into();
        assert!(
            matches!(sonda_err, SondaError::Runtime(_)),
            "ScenariosFailed must convert to SondaError::Runtime"
        );
    }

    // ---- config feature gate tests --------------------------------------------

    /// Config types are constructible in code regardless of the `config` feature.
    /// This test runs with or without the feature enabled.
    #[test]
    fn config_types_constructible_without_yaml_parsing() {
        use crate::config::{BaseScheduleConfig, ScenarioConfig};
        use crate::encoder::EncoderConfig;
        use crate::generator::GeneratorConfig;
        use crate::sink::SinkConfig;

        let _config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "test".to_string(),
                rate: 10.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        };
    }

    /// YAML deserialization is available when the `config` feature is active.
    #[cfg(feature = "config")]
    #[test]
    fn config_feature_enables_yaml_deserialization() {
        use crate::config::ScenarioConfig;

        let yaml = r#"
name: test
rate: 10
generator:
  type: constant
  value: 1.0
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml)
            .expect("YAML deserialization must work with config feature");
        assert_eq!(config.name, "test");
    }

    /// EncoderConfig, SinkConfig, and GeneratorConfig are all constructible
    /// without deserialization and can be passed to their respective factory functions.
    #[test]
    fn factory_functions_work_without_deserialization() {
        use crate::encoder::{create_encoder, EncoderConfig};
        use crate::generator::{create_generator, GeneratorConfig};
        use crate::sink::{create_sink, SinkConfig};

        let gen_config = GeneratorConfig::Constant { value: 42.0 };
        let gen = create_generator(&gen_config, 1.0).expect("generator factory must succeed");
        assert_eq!(gen.value(0), 42.0);

        let enc_config = EncoderConfig::PrometheusText { precision: None };
        let _enc = create_encoder(&enc_config).expect("encoder factory must succeed");

        let sink_config = SinkConfig::Stdout;
        let _sink = create_sink(&sink_config, None).expect("sink factory must succeed");
    }

    #[test]
    fn sonda_error_sink_display_includes_io_context() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let err = SondaError::Sink(io_err);
        let msg = format!("{err}");
        assert!(
            msg.contains("sink error"),
            "Sink variant display must include 'sink error', got: {msg}"
        );
        assert!(
            msg.contains("pipe broke"),
            "Sink variant display must include the I/O error message, got: {msg}"
        );
    }

    // ---- Contract: error types are Send + Sync --------------------------------

    #[test]
    fn error_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SondaError>();
        assert_send_sync::<ConfigError>();
        assert_send_sync::<GeneratorError>();
        assert_send_sync::<EncoderError>();
        assert_send_sync::<RuntimeError>();
    }
}
