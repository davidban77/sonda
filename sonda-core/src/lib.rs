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
pub use config::LogScenarioConfig;
pub use model::log::LogEvent;
pub use model::log::Severity;
pub use model::metric::Labels;
pub use model::metric::MetricEvent;

/// Top-level error type for sonda-core.
#[derive(Debug, thiserror::Error)]
pub enum SondaError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("encoder error: {0}")]
    Encoder(String),

    #[error("sink error: {0}")]
    Sink(#[from] std::io::Error),

    #[error("generator error: {0}")]
    Generator(String),
}
