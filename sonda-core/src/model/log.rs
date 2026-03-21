//! Log event model.
//!
//! Defines [`LogEvent`] and [`Severity`] — the canonical in-memory representation
//! of a structured log entry. Format-agnostic: encoding to JSON Lines or Syslog is
//! the encoder's concern, not this module's.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The severity level of a log event.
///
/// Variants map to the conventional log severity ladder. Serializes to and from
/// lowercase strings (e.g., `"info"`, `"error"`) for YAML and JSON compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Extremely detailed diagnostic information.
    Trace,
    /// Diagnostic information useful during development.
    Debug,
    /// General informational messages.
    Info,
    /// Potentially harmful situations that warrant attention.
    Warn,
    /// Error events that may allow the application to continue.
    Error,
    /// Severe error events that will likely cause the application to abort.
    Fatal,
}

/// A structured log entry with a timestamp, severity, message, and arbitrary fields.
///
/// Fields are stored in a [`BTreeMap`] so that key order is deterministic across
/// platforms and serialization round-trips.
#[derive(Debug, Clone)]
pub struct LogEvent {
    /// The time at which the event was generated.
    pub timestamp: SystemTime,
    /// The severity level of the event.
    pub severity: Severity,
    /// The human-readable log message.
    pub message: String,
    /// Arbitrary key-value metadata attached to the event.
    pub fields: BTreeMap<String, String>,
}

impl LogEvent {
    /// Create a new [`LogEvent`] with the current system time as its timestamp.
    ///
    /// # Arguments
    ///
    /// * `severity` — The severity level.
    /// * `message` — The human-readable message.
    /// * `fields` — Arbitrary key-value metadata.
    pub fn new(severity: Severity, message: String, fields: BTreeMap<String, String>) -> Self {
        Self {
            timestamp: SystemTime::now(),
            severity,
            message,
            fields,
        }
    }

    /// Create a [`LogEvent`] with an explicit timestamp.
    ///
    /// Useful for deterministic testing and log replay scenarios where the original
    /// timestamp must be preserved.
    ///
    /// # Arguments
    ///
    /// * `timestamp` — The exact timestamp to record.
    /// * `severity` — The severity level.
    /// * `message` — The human-readable message.
    /// * `fields` — Arbitrary key-value metadata.
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
