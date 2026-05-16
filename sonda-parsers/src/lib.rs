//! Parsers that convert external byte streams into the canonical Sonda log CSV
//! plus a runnable v2 scenario YAML that points at it.
//!
//! The CSV shape matches what `sonda_core::generator::log_csv_replay` consumes:
//! `timestamp,severity,message[,...field_columns]`. Field columns appear in
//! alphabetical order after the three named columns.

pub mod canonical;
pub mod rawlog;

use std::path::PathBuf;

/// Errors produced by `sonda-parsers`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParsersError {
    #[error("input file {path:?} could not be read")]
    InputRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("output file {path:?} could not be written")]
    OutputWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unknown format {name:?}: must be one of {known:?}")]
    UnknownFormat {
        name: String,
        known: Vec<&'static str>,
    },

    #[error("input file {path:?} contains no parseable rows")]
    EmptyInput { path: PathBuf },

    #[error("invalid --delta-seconds {value}: must be a finite positive number")]
    InvalidDelta { value: f64 },

    #[error("invalid timestamp on line {line}: {reason}")]
    InvalidTimestamp { line: usize, reason: String },

    #[error("output path {path:?} has no parent directory")]
    OutputHasNoParent { path: PathBuf },

    #[error("yaml serialization failed")]
    YamlSerialize(#[from] serde_yaml_ng::Error),

    #[error(transparent)]
    Sonda(#[from] sonda_core::SondaError),
}
