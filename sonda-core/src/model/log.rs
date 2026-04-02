//! Log event model.
//!
//! Defines [`LogEvent`] and [`Severity`] — the canonical in-memory representation
//! of a structured log entry. Format-agnostic: encoding to JSON Lines or Syslog is
//! the encoder's concern, not this module's.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::Serialize;

use crate::model::metric::Labels;

/// The severity level of a log event.
///
/// Variants map to the conventional log severity ladder. Serializes to and from
/// lowercase strings (e.g., `"info"`, `"error"`) for YAML and JSON compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
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

/// A structured log entry with a timestamp, severity, message, labels, and arbitrary fields.
///
/// Labels are scenario-level key-value pairs (injected by the log runner).
/// Fields are event-level key-value metadata (produced by the generator).
/// Both are stored in sorted containers for deterministic serialization.
#[derive(Debug, Clone)]
pub struct LogEvent {
    /// The time at which the event was generated.
    pub timestamp: SystemTime,
    /// The severity level of the event.
    pub severity: Severity,
    /// The human-readable log message.
    pub message: String,
    /// Scenario-level static labels attached to every event in this scenario.
    pub labels: Labels,
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
    /// * `labels` — Scenario-level static labels.
    /// * `fields` — Arbitrary key-value metadata.
    pub fn new(
        severity: Severity,
        message: String,
        labels: Labels,
        fields: BTreeMap<String, String>,
    ) -> Self {
        Self {
            timestamp: SystemTime::now(),
            severity,
            message,
            labels,
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
    /// * `labels` — Scenario-level static labels.
    /// * `fields` — Arbitrary key-value metadata.
    pub fn with_timestamp(
        timestamp: SystemTime,
        severity: Severity,
        message: String,
        labels: Labels,
        fields: BTreeMap<String, String>,
    ) -> Self {
        Self {
            timestamp,
            severity,
            message,
            labels,
            fields,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    // -----------------------------------------------------------------------
    // LogEvent::new — creates event with current timestamp
    // -----------------------------------------------------------------------

    #[test]
    fn new_uses_current_timestamp() {
        let before = SystemTime::now();
        let event = LogEvent::new(
            Severity::Info,
            "hello".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        let after = SystemTime::now();

        assert!(
            event.timestamp >= before,
            "timestamp should not precede the call"
        );
        assert!(
            event.timestamp <= after,
            "timestamp should not exceed the call"
        );
    }

    #[test]
    fn new_stores_severity_message_and_fields() {
        let mut fields = BTreeMap::new();
        fields.insert("host".to_string(), "web-01".to_string());

        let event = LogEvent::new(
            Severity::Error,
            "connection failed".to_string(),
            Labels::default(),
            fields,
        );

        assert_eq!(event.severity, Severity::Error);
        assert_eq!(event.message, "connection failed");
        assert_eq!(event.fields.get("host").map(String::as_str), Some("web-01"));
    }

    #[test]
    fn new_with_empty_fields_succeeds() {
        let event = LogEvent::new(
            Severity::Debug,
            "empty".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        assert!(event.fields.is_empty());
    }

    // -----------------------------------------------------------------------
    // LogEvent::with_timestamp — uses exact provided timestamp
    // -----------------------------------------------------------------------

    #[test]
    fn with_timestamp_uses_exact_provided_timestamp() {
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let event = LogEvent::with_timestamp(
            ts,
            Severity::Warn,
            "test message".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );

        assert_eq!(
            event.timestamp, ts,
            "timestamp must be exactly the one provided"
        );
    }

    #[test]
    fn with_timestamp_stores_all_fields_correctly() {
        let ts = UNIX_EPOCH + Duration::from_secs(42);
        let mut fields = BTreeMap::new();
        fields.insert("service".to_string(), "api".to_string());
        fields.insert("region".to_string(), "us-east-1".to_string());

        let event = LogEvent::with_timestamp(
            ts,
            Severity::Fatal,
            "system crash".to_string(),
            Labels::default(),
            fields,
        );

        assert_eq!(event.timestamp, ts);
        assert_eq!(event.severity, Severity::Fatal);
        assert_eq!(event.message, "system crash");
        assert_eq!(event.fields.get("service").map(String::as_str), Some("api"));
        assert_eq!(
            event.fields.get("region").map(String::as_str),
            Some("us-east-1")
        );
    }

    #[test]
    fn with_timestamp_at_unix_epoch_is_valid() {
        let event = LogEvent::with_timestamp(
            UNIX_EPOCH,
            Severity::Trace,
            "epoch".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        assert_eq!(event.timestamp, UNIX_EPOCH);
    }

    // -----------------------------------------------------------------------
    // LogEvent: fields use BTreeMap (sorted key order)
    // -----------------------------------------------------------------------

    #[test]
    fn fields_are_sorted_by_key() {
        let mut fields = BTreeMap::new();
        fields.insert("zebra".to_string(), "z".to_string());
        fields.insert("alpha".to_string(), "a".to_string());
        fields.insert("mango".to_string(), "m".to_string());

        let event = LogEvent::new(
            Severity::Info,
            "sorted".to_string(),
            Labels::default(),
            fields,
        );

        let keys: Vec<&str> = event.fields.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["alpha", "mango", "zebra"]);
    }

    // -----------------------------------------------------------------------
    // Severity: serializes to lowercase JSON
    // -----------------------------------------------------------------------

    #[test]
    fn severity_trace_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Trace).unwrap();
        assert_eq!(s, r#""trace""#);
    }

    #[test]
    fn severity_debug_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Debug).unwrap();
        assert_eq!(s, r#""debug""#);
    }

    #[test]
    fn severity_info_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Info).unwrap();
        assert_eq!(s, r#""info""#);
    }

    #[test]
    fn severity_warn_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Warn).unwrap();
        assert_eq!(s, r#""warn""#);
    }

    #[test]
    fn severity_error_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Error).unwrap();
        assert_eq!(s, r#""error""#);
    }

    #[test]
    fn severity_fatal_serializes_to_lowercase_json() {
        let s = serde_json::to_string(&Severity::Fatal).unwrap();
        assert_eq!(s, r#""fatal""#);
    }

    // -----------------------------------------------------------------------
    // Severity: deserializes from lowercase JSON
    // These tests require the `config` feature (Deserialize impl).
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_trace() {
        let s: Severity = serde_json::from_str(r#""trace""#).unwrap();
        assert_eq!(s, Severity::Trace);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_debug() {
        let s: Severity = serde_json::from_str(r#""debug""#).unwrap();
        assert_eq!(s, Severity::Debug);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_info() {
        let s: Severity = serde_json::from_str(r#""info""#).unwrap();
        assert_eq!(s, Severity::Info);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_warn() {
        let s: Severity = serde_json::from_str(r#""warn""#).unwrap();
        assert_eq!(s, Severity::Warn);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_error() {
        let s: Severity = serde_json::from_str(r#""error""#).unwrap();
        assert_eq!(s, Severity::Error);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_deserializes_from_lowercase_fatal() {
        let s: Severity = serde_json::from_str(r#""fatal""#).unwrap();
        assert_eq!(s, Severity::Fatal);
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_rejects_uppercase_deserialization() {
        let result: Result<Severity, _> = serde_json::from_str(r#""INFO""#);
        assert!(
            result.is_err(),
            "uppercase severity string must be rejected"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_rejects_unknown_variant() {
        let result: Result<Severity, _> = serde_json::from_str(r#""critical""#);
        assert!(result.is_err(), "unknown severity variant must be rejected");
    }

    // -----------------------------------------------------------------------
    // Severity: serializes to lowercase YAML
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn severity_info_serializes_to_lowercase_yaml() {
        let s = serde_yaml::to_string(&Severity::Info).unwrap();
        assert!(s.trim() == "info", "expected 'info', got: {s}");
    }

    #[cfg(feature = "config")]
    #[test]
    fn severity_error_serializes_to_lowercase_yaml() {
        let s = serde_yaml::to_string(&Severity::Error).unwrap();
        assert!(s.trim() == "error", "expected 'error', got: {s}");
    }

    // -----------------------------------------------------------------------
    // Severity: Send + Sync contract
    // -----------------------------------------------------------------------

    #[test]
    fn severity_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Severity>();
    }

    // -----------------------------------------------------------------------
    // LogEvent: Send + Sync contract
    // -----------------------------------------------------------------------

    #[test]
    fn log_event_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LogEvent>();
    }

    // -----------------------------------------------------------------------
    // LogEvent: Clone produces independent copies
    // -----------------------------------------------------------------------

    #[test]
    fn log_event_clone_is_independent() {
        let ts = UNIX_EPOCH + Duration::from_secs(1000);
        let mut fields = BTreeMap::new();
        fields.insert("k".to_string(), "v".to_string());

        let original = LogEvent::with_timestamp(
            ts,
            Severity::Info,
            "msg".to_string(),
            Labels::default(),
            fields,
        );
        let mut cloned = original.clone();

        cloned.message = "different".to_string();
        cloned.fields.insert("k".to_string(), "changed".to_string());

        assert_eq!(original.message, "msg");
        assert_eq!(original.fields.get("k").map(String::as_str), Some("v"));
    }

    // -----------------------------------------------------------------------
    // LogEvent: labels field — stores scenario-level static labels
    // -----------------------------------------------------------------------

    #[test]
    fn new_stores_labels_correctly() {
        let labels = Labels::from_pairs(&[("device", "wlan0"), ("hostname", "router-01")]).unwrap();
        let event = LogEvent::new(Severity::Info, "test".to_string(), labels, BTreeMap::new());

        assert_eq!(event.labels.len(), 2);
        let label_pairs: Vec<(&String, &String)> = event.labels.iter().collect();
        assert_eq!(label_pairs[0].0.as_str(), "device");
        assert_eq!(label_pairs[0].1.as_str(), "wlan0");
        assert_eq!(label_pairs[1].0.as_str(), "hostname");
        assert_eq!(label_pairs[1].1.as_str(), "router-01");
    }

    #[test]
    fn with_timestamp_stores_labels_correctly() {
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let labels = Labels::from_pairs(&[("env", "staging"), ("region", "us_west")]).unwrap();
        let event = LogEvent::with_timestamp(
            ts,
            Severity::Warn,
            "warning event".to_string(),
            labels,
            BTreeMap::new(),
        );

        assert_eq!(event.labels.len(), 2);
        let label_pairs: Vec<(&String, &String)> = event.labels.iter().collect();
        assert_eq!(label_pairs[0].0.as_str(), "env");
        assert_eq!(label_pairs[0].1.as_str(), "staging");
        assert_eq!(label_pairs[1].0.as_str(), "region");
        assert_eq!(label_pairs[1].1.as_str(), "us_west");
    }

    #[test]
    fn log_event_clone_preserves_labels() {
        let ts = UNIX_EPOCH + Duration::from_secs(1000);
        let labels = Labels::from_pairs(&[("service", "api"), ("zone", "eu1")]).unwrap();
        let original = LogEvent::with_timestamp(
            ts,
            Severity::Error,
            "cloned".to_string(),
            labels,
            BTreeMap::new(),
        );

        let cloned = original.clone();

        assert_eq!(cloned.labels.len(), 2);
        let original_pairs: Vec<(&String, &String)> = original.labels.iter().collect();
        let cloned_pairs: Vec<(&String, &String)> = cloned.labels.iter().collect();
        assert_eq!(original_pairs, cloned_pairs);
    }

    #[test]
    fn new_with_empty_labels_has_no_labels() {
        let event = LogEvent::new(
            Severity::Info,
            "no labels".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        assert!(event.labels.is_empty());
        assert_eq!(event.labels.len(), 0);
    }
}
