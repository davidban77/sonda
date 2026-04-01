//! sonda-core — the engine for synthetic telemetry generation.
//!
//! This crate owns all domain logic: telemetry models, value generators,
//! schedulers, encoders, and sinks. The CLI and HTTP server are thin layers
//! that call into this library.

pub mod config;
pub mod encoder;
pub mod generator;
pub mod model;
pub mod schedule;
pub mod sink;

pub use config::BurstConfig;
pub use config::CardinalitySpikeConfig;
pub use config::LogScenarioConfig;
pub use config::MultiScenarioConfig;
pub use config::ScenarioEntry;
pub use config::SpikeStrategy;
pub use model::log::LogEvent;
pub use model::log::Severity;
pub use model::metric::Labels;
pub use model::metric::MetricEvent;
pub use schedule::handle::ScenarioHandle;
pub use schedule::launch::{launch_scenario, validate_entry};
pub use schedule::stats::ScenarioStats;

/// Top-level error type for sonda-core.
///
/// Each variant represents a distinct origin for errors in the crate. The
/// `Sink` variant wraps [`std::io::Error`] without a blanket `#[from]`
/// conversion — all I/O errors must be explicitly mapped to the correct
/// variant at the call site. This prevents generator or config I/O errors
/// from being misclassified as sink errors.
#[derive(Debug, thiserror::Error)]
pub enum SondaError {
    /// An error in scenario configuration (invalid values, missing fields).
    #[error("configuration error: {0}")]
    Config(String),

    /// An error during event encoding (protobuf, format issues).
    #[error("encoder error: {0}")]
    Encoder(String),

    /// An I/O error originating from a sink (stdout, file, TCP, UDP, HTTP).
    ///
    /// This variant does **not** use `#[from] std::io::Error` because not all
    /// I/O errors originate from sinks. Generator file reads, for example,
    /// produce [`SondaError::Generator`] instead.
    #[error("sink error: {0}")]
    Sink(std::io::Error),

    /// An error from a generator (file not found, invalid data).
    #[error("generator error: {0}")]
    Generator(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- SondaError variant discrimination ------------------------------------

    #[test]
    fn io_error_does_not_auto_convert_to_sonda_error() {
        // Verify that removing #[from] means there is no From<io::Error> impl.
        // We assert this at the type level: SondaError::Sink must be constructed
        // explicitly.
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
    fn missing_csv_file_produces_config_error_not_sink() {
        let result = generator::csv_replay::CsvReplayGenerator::new(
            "/nonexistent/path/for/data.csv",
            0,
            false,
            true,
        );
        match result {
            Err(ref err) => {
                assert!(
                    matches!(err, SondaError::Config(_)),
                    "missing CSV file must produce Config variant, got: {err:?}"
                );
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
        let err = SondaError::Generator("cannot read file: no such file".to_string());
        let msg = format!("{err}");
        assert!(
            msg.contains("generator error"),
            "Generator variant display must include 'generator error', got: {msg}"
        );
        assert!(
            msg.contains("cannot read file"),
            "Generator variant display must include the inner message, got: {msg}"
        );
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
}
