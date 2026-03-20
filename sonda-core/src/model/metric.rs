//! Canonical metric event representation.
//!
//! Format-agnostic — encoding to Prometheus, Influx, or JSON is the encoder's concern.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::SondaError;

/// Returns `true` if `s` is a valid Prometheus label key.
///
/// Valid label keys match `[a-zA-Z_][a-zA-Z0-9_]*` and must not be empty.
fn is_valid_label_key(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    // First character: letter or underscore
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    // Remaining characters: letter, digit, or underscore
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Returns `true` if `s` is a valid Prometheus metric name.
///
/// Valid metric names match `[a-zA-Z_:][a-zA-Z0-9_:]*` and must not be empty.
pub(crate) fn is_valid_metric_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    // First character: letter, underscore, or colon
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    // Remaining characters: letter, digit, underscore, or colon
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// An ordered, deduplicated set of string label key-value pairs.
///
/// Keys are stored in sorted order (BTreeMap guarantee) and validated at construction time.
#[derive(Debug, Clone, PartialEq)]
pub struct Labels {
    inner: BTreeMap<String, String>,
}

impl Labels {
    /// Create a new label set from key-value pairs without validation.
    ///
    /// Duplicate keys are resolved by last-write-wins. Prefer [`Labels::from_pairs`]
    /// for validated construction.
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        let inner = pairs.into_iter().collect();
        Self { inner }
    }

    /// Create a validated label set from string slice pairs.
    ///
    /// Validates that each key matches `[a-zA-Z_][a-zA-Z0-9_]*`. Returns
    /// [`SondaError::Config`] if any key is invalid, including the invalid key
    /// in the error message.
    ///
    /// Duplicate keys are resolved by last-write-wins.
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Result<Self, SondaError> {
        let mut inner = BTreeMap::new();
        for (key, value) in pairs {
            if !is_valid_label_key(key) {
                return Err(SondaError::Config(format!(
                    "invalid label key {:?}: must match [a-zA-Z_][a-zA-Z0-9_]*",
                    key
                )));
            }
            inner.insert(key.to_string(), value.to_string());
        }
        Ok(Self { inner })
    }

    /// Returns an iterator over the label key-value pairs in sorted key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.inner.iter()
    }

    /// Returns the number of labels in this set.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if this label set contains no labels.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// A single timestamped metric sample.
///
/// Carries a metric name, `f64` value, a set of string label pairs, and a timestamp.
/// The metric name is validated at construction time.
#[derive(Debug, Clone)]
pub struct MetricEvent {
    /// The metric name.
    pub name: String,
    /// The numeric value of this sample.
    pub value: f64,
    /// The label set associated with this sample.
    pub labels: Labels,
    /// The time at which this sample was recorded.
    pub timestamp: SystemTime,
}

impl MetricEvent {
    /// Construct a new `MetricEvent` with the current system time as the timestamp.
    ///
    /// Validates that `name` matches `[a-zA-Z_:][a-zA-Z0-9_:]*`. Returns
    /// [`SondaError::Config`] if the name is invalid.
    pub fn new(name: String, value: f64, labels: Labels) -> Result<Self, SondaError> {
        if !is_valid_metric_name(&name) {
            return Err(SondaError::Config(format!(
                "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
                name
            )));
        }
        Ok(Self {
            name,
            value,
            labels,
            timestamp: SystemTime::now(),
        })
    }

    /// Construct a new `MetricEvent` with an explicit timestamp.
    ///
    /// Useful for deterministic testing and replay scenarios. Validates the metric
    /// name with the same rules as [`MetricEvent::new`].
    pub fn with_timestamp(
        name: String,
        value: f64,
        labels: Labels,
        timestamp: SystemTime,
    ) -> Result<Self, SondaError> {
        if !is_valid_metric_name(&name) {
            return Err(SondaError::Config(format!(
                "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
                name
            )));
        }
        Ok(Self {
            name,
            value,
            labels,
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    // --- Labels::from_pairs happy path ---

    #[test]
    fn from_pairs_with_single_valid_pair_returns_ok() {
        let labels = Labels::from_pairs(&[("host", "server1")]).unwrap();
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn from_pairs_with_multiple_valid_pairs_returns_ok() {
        let labels =
            Labels::from_pairs(&[("host", "server1"), ("zone", "eu1"), ("env", "prod")]).unwrap();
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn from_pairs_stores_correct_values() {
        let labels = Labels::from_pairs(&[("host", "server1"), ("zone", "eu1")]).unwrap();
        let mut iter = labels.iter();
        let (k1, v1) = iter.next().unwrap();
        let (k2, v2) = iter.next().unwrap();
        // BTreeMap sorts by key: "host" < "zone"
        assert_eq!(k1, "host");
        assert_eq!(v1, "server1");
        assert_eq!(k2, "zone");
        assert_eq!(v2, "eu1");
    }

    #[test]
    fn from_pairs_with_underscore_leading_key_returns_ok() {
        let labels = Labels::from_pairs(&[("_internal", "value")]).unwrap();
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn from_pairs_with_mixed_alphanumeric_key_returns_ok() {
        let labels = Labels::from_pairs(&[("label_key_123", "value")]).unwrap();
        assert_eq!(labels.len(), 1);
    }

    // --- Labels::from_pairs error cases ---

    #[test]
    fn from_pairs_with_digit_leading_key_returns_config_error() {
        let err = Labels::from_pairs(&[("1bad", "value")]).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref msg) if msg.contains("1bad")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_with_hyphen_in_key_returns_config_error() {
        let err = Labels::from_pairs(&[("bad-key", "value")]).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref msg) if msg.contains("bad-key")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_with_empty_key_returns_config_error() {
        let err = Labels::from_pairs(&[("", "value")]).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_with_space_in_key_returns_config_error() {
        let err = Labels::from_pairs(&[("bad key", "value")]).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref msg) if msg.contains("bad key")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_error_message_includes_invalid_key() {
        let err = Labels::from_pairs(&[("9invalid", "v")]).unwrap_err();
        let SondaError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(
            msg.contains("9invalid"),
            "message missing invalid key: {msg}"
        );
    }

    // --- Labels::from_pairs duplicate key handling ---

    #[test]
    fn from_pairs_duplicate_key_last_write_wins() {
        let labels = Labels::from_pairs(&[("host", "first"), ("host", "second")]).unwrap();
        assert_eq!(labels.len(), 1);
        let (_, v) = labels.iter().next().unwrap();
        assert_eq!(v, "second");
    }

    // --- Labels::len and is_empty ---

    #[test]
    fn len_returns_count_of_unique_keys() {
        let labels = Labels::from_pairs(&[("a", "1"), ("b", "2"), ("c", "3")]).unwrap();
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn is_empty_returns_true_for_empty_label_set() {
        let labels = Labels::from_pairs(&[]).unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn is_empty_returns_false_for_nonempty_label_set() {
        let labels = Labels::from_pairs(&[("k", "v")]).unwrap();
        assert!(!labels.is_empty());
    }

    // --- Labels sorted order ---

    #[test]
    fn labels_iter_yields_keys_in_sorted_order() {
        let labels =
            Labels::from_pairs(&[("zone", "eu1"), ("host", "server1"), ("env", "prod")]).unwrap();
        let keys: Vec<&str> = labels.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["env", "host", "zone"]);
    }

    // --- MetricEvent::new happy path ---

    #[test]
    fn metric_event_new_with_valid_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("up".to_string(), 1.0, labels).unwrap();
        assert_eq!(event.name, "up");
        assert_eq!(event.value, 1.0);
    }

    #[test]
    fn metric_event_new_with_underscored_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("http_requests_total".to_string(), 42.0, labels).unwrap();
        assert_eq!(event.name, "http_requests_total");
    }

    #[test]
    fn metric_event_new_with_double_underscore_prefix_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("__internal".to_string(), 0.0, labels).unwrap();
        assert_eq!(event.name, "__internal");
    }

    #[test]
    fn metric_event_new_with_colon_in_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("my:metric".to_string(), 0.0, labels).unwrap();
        assert_eq!(event.name, "my:metric");
    }

    #[test]
    fn metric_event_new_with_colon_leading_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new(":colon_first".to_string(), 0.0, labels).unwrap();
        assert_eq!(event.name, ":colon_first");
    }

    // --- MetricEvent::new error cases ---

    #[test]
    fn metric_event_new_with_digit_leading_name_returns_config_error() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("123bad".to_string(), 0.0, labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref msg) if msg.contains("123bad")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn metric_event_new_with_dash_in_name_returns_config_error() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("has-dash".to_string(), 0.0, labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref msg) if msg.contains("has-dash")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn metric_event_new_with_empty_name_returns_config_error() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("".to_string(), 0.0, labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn metric_event_new_error_message_includes_invalid_name() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("123bad".to_string(), 0.0, labels).unwrap_err();
        let SondaError::Config(msg) = err else {
            panic!("expected Config error");
        };
        assert!(
            msg.contains("123bad"),
            "message missing invalid name: {msg}"
        );
    }

    // --- MetricEvent::with_timestamp ---

    #[test]
    fn with_timestamp_stores_exact_provided_timestamp() {
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();
        assert_eq!(event.timestamp, ts);
    }

    #[test]
    fn with_timestamp_stores_epoch_zero_timestamp() {
        let ts = UNIX_EPOCH;
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::with_timestamp("up".to_string(), 0.0, labels, ts).unwrap();
        assert_eq!(event.timestamp, UNIX_EPOCH);
    }

    #[test]
    fn with_timestamp_validates_name_same_as_new() {
        let ts = UNIX_EPOCH;
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::with_timestamp("123bad".to_string(), 0.0, labels, ts).unwrap_err();
        assert!(matches!(err, SondaError::Config(_)));
    }

    #[test]
    fn with_timestamp_stores_name_and_value_correctly() {
        let ts = UNIX_EPOCH + Duration::from_millis(500);
        let labels = Labels::from_pairs(&[("env", "test")]).unwrap();
        let event = MetricEvent::with_timestamp("my_metric".to_string(), 3.14, labels, ts).unwrap();
        assert_eq!(event.name, "my_metric");
        assert_eq!(event.value, 3.14);
    }

    // --- MetricEvent::new uses current time (not UNIX_EPOCH) ---

    #[test]
    fn metric_event_new_timestamp_is_after_unix_epoch() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("up".to_string(), 1.0, labels).unwrap();
        assert!(
            event.timestamp > UNIX_EPOCH,
            "timestamp should be after UNIX_EPOCH"
        );
    }

    // --- Send + Sync contract tests ---

    #[test]
    fn labels_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Labels>();
    }

    #[test]
    fn metric_event_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricEvent>();
    }
}
