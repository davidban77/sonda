//! Log event model — structured log line representation.
//!
//! Format-agnostic. Encoding to JSON Lines, Syslog, or other formats
//! is the encoder's concern.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The severity level of a log event.
///
/// Variants are ordered from least to most severe. Serializes to lowercase
/// strings for interoperability with common log pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Extremely fine-grained diagnostic information.
    Trace,
    /// Internal diagnostic information useful for debugging.
    Debug,
    /// Informational messages indicating normal operation.
    Info,
    /// Potentially harmful situations that deserve attention.
    Warn,
    /// Error events that might still allow the application to continue.
    Error,
    /// Very severe events that will likely cause the application to abort.
    Fatal,
}

/// A single structured log event.
///
/// Carries a timestamp, severity level, human-readable message, and an ordered
/// set of structured key-value fields.
#[derive(Debug, Clone)]
pub struct LogEvent {
    /// The time at which this log event occurred.
    pub timestamp: SystemTime,
    /// The severity level of this event.
    pub severity: Severity,
    /// The human-readable log message.
    pub message: String,
    /// Structured key-value fields associated with this event.
    ///
    /// Uses `BTreeMap` to guarantee consistent, sorted key ordering.
    pub fields: BTreeMap<String, String>,
}

impl LogEvent {
    /// Construct a new `LogEvent` with the current system time as the timestamp.
    pub fn new(severity: Severity, message: String, fields: BTreeMap<String, String>) -> Self {
        Self {
            timestamp: SystemTime::now(),
            severity,
            message,
            fields,
        }
    }

    /// Construct a new `LogEvent` with an explicit timestamp.
    ///
    /// Useful for deterministic testing and replay scenarios.
    pub fn with_timestamp(
        timestamp: SystemTime,
        severity: Severity,
        message: String,
        fields: BTreeMap<String, String>,
    ) -> Self {
        Self {
            timestamp,
            severity,
            message,
            fields,
        }
    }
}
