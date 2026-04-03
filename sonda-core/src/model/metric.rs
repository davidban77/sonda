//! Canonical metric event representation.
//!
//! Format-agnostic — encoding to Prometheus, Influx, or JSON is the encoder's concern.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::time::SystemTime;

use crate::{ConfigError, SondaError};

/// Returns `true` if `s` is a valid Prometheus label key.
///
/// Valid label keys match `[a-zA-Z_][a-zA-Z0-9_]*` and must not be empty.
pub(crate) fn is_valid_label_key(s: &str) -> bool {
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

/// A metric name that has been validated at construction time.
///
/// Wraps an `Arc<str>` containing a name that matches `[a-zA-Z_:][a-zA-Z0-9_:]*`.
/// Once constructed, the name is guaranteed valid — no further validation is
/// needed. This makes [`MetricEvent::from_parts`] safe by construction: the type
/// system enforces the invariant instead of a doc-level safety contract.
///
/// Cloning is O(1) — just a reference-count bump on the inner `Arc<str>`.
///
/// Dereferences to `&str` for seamless use in encoders and formatting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValidatedMetricName(Arc<str>);

impl ValidatedMetricName {
    /// Validate and construct a `ValidatedMetricName`.
    ///
    /// Returns [`SondaError::Config`] if the name does not match
    /// `[a-zA-Z_:][a-zA-Z0-9_:]*` or is empty.
    pub fn new(name: &str) -> Result<Self, SondaError> {
        if !is_valid_metric_name(name) {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "invalid metric name {:?}: must match [a-zA-Z_:][a-zA-Z0-9_:]*",
                name
            ))));
        }
        Ok(Self(Arc::from(name)))
    }

    /// Returns the underlying `Arc<str>` for pointer-equality checks.
    ///
    /// This is useful in tests that verify Arc sharing semantics (e.g.,
    /// confirming that multiple events share the same name allocation).
    pub fn arc(&self) -> &Arc<str> {
        &self.0
    }
}

impl Deref for ValidatedMetricName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ValidatedMetricName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValidatedMetricName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An ordered, deduplicated set of string label key-value pairs.
///
/// Keys are stored in sorted order (BTreeMap guarantee) and validated at construction time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Labels {
    inner: BTreeMap<String, String>,
}

impl Labels {
    /// Create a new label set from key-value pairs without validation.
    ///
    /// This constructor is available only in test builds. Production code
    /// should use [`Labels::from_pairs`] which validates label keys.
    ///
    /// Duplicate keys are resolved by last-write-wins.
    #[cfg(test)]
    pub(crate) fn new(pairs: Vec<(String, String)>) -> Self {
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
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "invalid label key {:?}: must match [a-zA-Z_][a-zA-Z0-9_]*",
                    key
                ))));
            }
            inner.insert(key.to_string(), value.to_string());
        }
        Ok(Self { inner })
    }

    /// Returns an iterator over the label key-value pairs in sorted key order.
    ///
    /// Returns `(&str, &str)` rather than `(&String, &String)` for idiomatic Rust
    /// usage. The underlying `BTreeMap` iteration order is preserved.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Returns the number of labels in this set.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if this label set contains no labels.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Insert a key-value pair into this label set without validation.
    ///
    /// The caller is responsible for ensuring the key is a valid Prometheus
    /// label key. This method is intended for use by the schedule runner
    /// when injecting spike labels from pre-validated config.
    ///
    /// If the key already exists, its value is overwritten.
    pub fn insert(&mut self, key: String, value: String) {
        self.inner.insert(key, value);
    }
}

/// A single timestamped metric sample.
///
/// Carries a metric name, `f64` value, a set of string label pairs, and a timestamp.
/// The metric name is validated at construction time via [`ValidatedMetricName`].
///
/// The `name` field uses [`ValidatedMetricName`] (which wraps `Arc<str>`) and the
/// `labels` field uses `Arc<Labels>` so that cloning a `MetricEvent` is O(1) — just
/// reference-count bumps — rather than deep-copying the name string and the full
/// label `BTreeMap`. This matters in the metric runner hot path where the name and
/// labels are invariant across ticks.
#[derive(Debug, Clone)]
pub struct MetricEvent {
    /// The metric name, validated at construction and reference-counted for O(1)
    /// cloning in the hot path.
    pub name: ValidatedMetricName,
    /// The numeric value of this sample.
    pub value: f64,
    /// The label set associated with this sample (reference-counted for O(1)
    /// cloning when no cardinality spike mutation is needed).
    pub labels: Arc<Labels>,
    /// The time at which this sample was recorded.
    pub timestamp: SystemTime,
}

impl MetricEvent {
    /// Construct a new `MetricEvent` with the current system time as the timestamp.
    ///
    /// Validates that `name` matches `[a-zA-Z_:][a-zA-Z0-9_:]*`. Returns
    /// [`SondaError::Config`] if the name is invalid.
    ///
    /// The `name` is stored as a [`ValidatedMetricName`] (wrapping `Arc<str>`) and
    /// `labels` as `Arc<Labels>` for O(1) cloning. To avoid per-event validation
    /// and allocation in hot loops, prefer [`MetricEvent::from_parts`] with a
    /// pre-validated [`ValidatedMetricName`] and `Arc<Labels>`.
    pub fn new(name: String, value: f64, labels: Labels) -> Result<Self, SondaError> {
        let validated = ValidatedMetricName::new(&name)?;
        Ok(Self {
            name: validated,
            value,
            labels: Arc::new(labels),
            timestamp: SystemTime::now(),
        })
    }

    /// Construct a new `MetricEvent` with an explicit timestamp.
    ///
    /// Useful for deterministic testing and replay scenarios. Validates the metric
    /// name with the same rules as [`MetricEvent::new`].
    ///
    /// The `name` is stored as a [`ValidatedMetricName`] (wrapping `Arc<str>`) and
    /// `labels` as `Arc<Labels>` for O(1) cloning. To avoid per-event validation
    /// and allocation in hot loops, prefer [`MetricEvent::from_parts`] with a
    /// pre-validated [`ValidatedMetricName`] and `Arc<Labels>`.
    pub fn with_timestamp(
        name: String,
        value: f64,
        labels: Labels,
        timestamp: SystemTime,
    ) -> Result<Self, SondaError> {
        let validated = ValidatedMetricName::new(&name)?;
        Ok(Self {
            name: validated,
            value,
            labels: Arc::new(labels),
            timestamp,
        })
    }

    /// Construct a `MetricEvent` from pre-validated, pre-shared parts.
    ///
    /// This is the hot-path constructor used by the metric runner. The caller
    /// provides a [`ValidatedMetricName`] and a pre-built `Arc<Labels>`,
    /// avoiding both name validation and heap allocation on every tick.
    ///
    /// Because `name` is a [`ValidatedMetricName`], the caller cannot pass an
    /// invalid metric name — the type system enforces the invariant at compile
    /// time.
    pub fn from_parts(
        name: ValidatedMetricName,
        value: f64,
        labels: Arc<Labels>,
        timestamp: SystemTime,
    ) -> Self {
        Self {
            name,
            value,
            labels,
            timestamp,
        }
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
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("1bad")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_with_hyphen_in_key_returns_config_error() {
        let err = Labels::from_pairs(&[("bad-key", "value")]).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("bad-key")),
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
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("bad key")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn from_pairs_error_message_includes_invalid_key() {
        let err = Labels::from_pairs(&[("9invalid", "v")]).unwrap_err();
        let SondaError::Config(ref e) = err else {
            panic!("expected Config error");
        };
        let msg = e.to_string();
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
        let keys: Vec<&str> = labels.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["env", "host", "zone"]);
    }

    // --- MetricEvent::new happy path ---

    #[test]
    fn metric_event_new_with_valid_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("up".to_string(), 1.0, labels).unwrap();
        assert_eq!(&*event.name, "up");
        assert_eq!(event.value, 1.0);
    }

    #[test]
    fn metric_event_new_with_underscored_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("http_requests_total".to_string(), 42.0, labels).unwrap();
        assert_eq!(&*event.name, "http_requests_total");
    }

    #[test]
    fn metric_event_new_with_double_underscore_prefix_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("__internal".to_string(), 0.0, labels).unwrap();
        assert_eq!(&*event.name, "__internal");
    }

    #[test]
    fn metric_event_new_with_colon_in_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("my:metric".to_string(), 0.0, labels).unwrap();
        assert_eq!(&*event.name, "my:metric");
    }

    #[test]
    fn metric_event_new_with_colon_leading_name_returns_ok() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new(":colon_first".to_string(), 0.0, labels).unwrap();
        assert_eq!(&*event.name, ":colon_first");
    }

    // --- MetricEvent::new error cases ---

    #[test]
    fn metric_event_new_with_digit_leading_name_returns_config_error() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("123bad".to_string(), 0.0, labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("123bad")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn metric_event_new_with_dash_in_name_returns_config_error() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let err = MetricEvent::new("has-dash".to_string(), 0.0, labels).unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("has-dash")),
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
        let SondaError::Config(ref e) = err else {
            panic!("expected Config error");
        };
        let msg = e.to_string();
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
        assert_eq!(&*event.name, "my_metric");
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

    // --- Labels::insert ---

    #[test]
    fn insert_adds_new_key() {
        let mut labels = Labels::from_pairs(&[("host", "server1")]).unwrap();
        labels.insert("zone".to_string(), "eu1".to_string());
        assert_eq!(labels.len(), 2);
    }

    #[test]
    fn insert_overwrites_existing_key() {
        let mut labels = Labels::from_pairs(&[("host", "server1")]).unwrap();
        labels.insert("host".to_string(), "server2".to_string());
        assert_eq!(labels.len(), 1);
        let (_, v) = labels.iter().next().unwrap();
        assert_eq!(v, "server2");
    }

    #[test]
    fn insert_maintains_sorted_order() {
        let mut labels = Labels::from_pairs(&[("b", "2")]).unwrap();
        labels.insert("a".to_string(), "1".to_string());
        labels.insert("c".to_string(), "3".to_string());
        let keys: Vec<&str> = labels.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_into_empty_labels() {
        let mut labels = Labels::default();
        labels.insert("key".to_string(), "value".to_string());
        assert_eq!(labels.len(), 1);
        let (k, v) = labels.iter().next().unwrap();
        assert_eq!(k, "key");
        assert_eq!(v, "value");
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

    // --- ValidatedMetricName ---

    #[test]
    fn validated_name_accepts_simple_name() {
        let name = ValidatedMetricName::new("up").unwrap();
        assert_eq!(&*name, "up");
    }

    #[test]
    fn validated_name_accepts_underscored_name() {
        let name = ValidatedMetricName::new("http_requests_total").unwrap();
        assert_eq!(&*name, "http_requests_total");
    }

    #[test]
    fn validated_name_accepts_colon_prefix() {
        let name = ValidatedMetricName::new(":colon_first").unwrap();
        assert_eq!(&*name, ":colon_first");
    }

    #[test]
    fn validated_name_accepts_name_with_colons() {
        let name = ValidatedMetricName::new("my:metric:name").unwrap();
        assert_eq!(&*name, "my:metric:name");
    }

    #[test]
    fn validated_name_rejects_digit_leading() {
        let err = ValidatedMetricName::new("123bad").unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("123bad")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validated_name_rejects_empty_string() {
        let err = ValidatedMetricName::new("").unwrap_err();
        assert!(
            matches!(err, SondaError::Config(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validated_name_rejects_dash_in_name() {
        let err = ValidatedMetricName::new("has-dash").unwrap_err();
        assert!(
            matches!(err, SondaError::Config(ref e) if e.to_string().contains("has-dash")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validated_name_deref_returns_str() {
        let name = ValidatedMetricName::new("test_metric").unwrap();
        let s: &str = &name;
        assert_eq!(s, "test_metric");
    }

    #[test]
    fn validated_name_as_ref_returns_str() {
        let name = ValidatedMetricName::new("test_metric").unwrap();
        let s: &str = name.as_ref();
        assert_eq!(s, "test_metric");
    }

    #[test]
    fn validated_name_display_shows_name() {
        let name = ValidatedMetricName::new("cpu_usage").unwrap();
        assert_eq!(format!("{name}"), "cpu_usage");
    }

    #[test]
    fn validated_name_clone_shares_arc_allocation() {
        let name = ValidatedMetricName::new("shared").unwrap();
        let cloned = name.clone();
        assert!(Arc::ptr_eq(name.arc(), cloned.arc()));
    }

    #[test]
    fn validated_name_eq_compares_by_value() {
        let name1 = ValidatedMetricName::new("up").unwrap();
        let name2 = ValidatedMetricName::new("up").unwrap();
        assert_eq!(name1, name2);
    }

    #[test]
    fn validated_name_ne_for_different_values() {
        let name1 = ValidatedMetricName::new("up").unwrap();
        let name2 = ValidatedMetricName::new("down").unwrap();
        assert_ne!(name1, name2);
    }

    #[test]
    fn validated_name_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ValidatedMetricName>();
    }

    #[test]
    fn validated_name_as_bytes_works_via_deref() {
        let name = ValidatedMetricName::new("metric").unwrap();
        assert_eq!(name.as_bytes(), b"metric");
    }

    // --- MetricEvent::from_parts ---

    #[test]
    fn from_parts_constructs_event_with_given_fields() {
        let name = ValidatedMetricName::new("http_requests_total").unwrap();
        let labels = Arc::new(Labels::from_pairs(&[("env", "prod")]).unwrap());
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let event = MetricEvent::from_parts(name.clone(), 42.0, Arc::clone(&labels), ts);
        assert_eq!(&*event.name, "http_requests_total");
        assert_eq!(event.value, 42.0);
        assert_eq!(event.labels.len(), 1);
        assert_eq!(event.timestamp, ts);
    }

    #[test]
    fn from_parts_requires_validated_name() {
        // ValidatedMetricName::new rejects invalid names, so from_parts
        // cannot be called with an invalid name — the type system prevents it.
        // This test documents that from_parts accepts ValidatedMetricName,
        // not a raw string or Arc<str>.
        let name = ValidatedMetricName::new("valid_metric").unwrap();
        let labels = Arc::new(Labels::default());
        let ts = UNIX_EPOCH;
        let event = MetricEvent::from_parts(name, 0.0, labels, ts);
        assert_eq!(&*event.name, "valid_metric");
    }

    #[test]
    fn from_parts_preserves_exact_timestamp() {
        let name = ValidatedMetricName::new("up").unwrap();
        let labels = Arc::new(Labels::default());
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_500);
        let event = MetricEvent::from_parts(name, 1.0, labels, ts);
        assert_eq!(event.timestamp, ts);
    }

    // --- Arc sharing semantics: clone is O(1) refcount bump ---

    #[test]
    fn name_arc_is_shared_across_cloned_events() {
        let name = ValidatedMetricName::new("up").unwrap();
        let labels = Arc::new(Labels::default());
        let ts = UNIX_EPOCH;
        let event1 = MetricEvent::from_parts(name.clone(), 1.0, Arc::clone(&labels), ts);
        let event2 = event1.clone();

        // Both events should point to the exact same heap allocation.
        assert!(Arc::ptr_eq(event1.name.arc(), event2.name.arc()));
    }

    #[test]
    fn labels_arc_is_shared_across_cloned_events() {
        let name = ValidatedMetricName::new("up").unwrap();
        let labels = Arc::new(Labels::from_pairs(&[("host", "srv1")]).unwrap());
        let ts = UNIX_EPOCH;
        let event1 = MetricEvent::from_parts(name.clone(), 1.0, Arc::clone(&labels), ts);
        let event2 = event1.clone();

        // Both events should share the same Labels allocation.
        assert!(Arc::ptr_eq(&event1.labels, &event2.labels));
    }

    #[test]
    fn name_arc_is_shared_between_from_parts_and_source() {
        let name = ValidatedMetricName::new("metric_name").unwrap();
        let labels = Arc::new(Labels::default());
        let ts = UNIX_EPOCH;
        let event = MetricEvent::from_parts(name.clone(), 0.0, Arc::clone(&labels), ts);

        // The event's name should share the same allocation as the source.
        assert!(Arc::ptr_eq(event.name.arc(), name.arc()));
    }

    #[test]
    fn labels_arc_is_shared_between_from_parts_and_source() {
        let name = ValidatedMetricName::new("up").unwrap();
        let labels = Arc::new(Labels::from_pairs(&[("a", "1"), ("b", "2")]).unwrap());
        let ts = UNIX_EPOCH;
        let event = MetricEvent::from_parts(name, 0.0, Arc::clone(&labels), ts);

        // The event's labels should share the same allocation as the source Arc.
        assert!(Arc::ptr_eq(&event.labels, &labels));
    }

    #[test]
    fn multiple_events_from_same_validated_name_share_allocation() {
        let name = ValidatedMetricName::new("shared_metric").unwrap();
        let labels = Arc::new(Labels::default());
        let ts = UNIX_EPOCH;

        let event1 = MetricEvent::from_parts(name.clone(), 1.0, Arc::clone(&labels), ts);
        let event2 = MetricEvent::from_parts(name.clone(), 2.0, Arc::clone(&labels), ts);
        let event3 = MetricEvent::from_parts(name.clone(), 3.0, Arc::clone(&labels), ts);

        // All three events share the same name and labels allocations.
        assert!(Arc::ptr_eq(event1.name.arc(), event2.name.arc()));
        assert!(Arc::ptr_eq(event2.name.arc(), event3.name.arc()));
        assert!(Arc::ptr_eq(&event1.labels, &event2.labels));
        assert!(Arc::ptr_eq(&event2.labels, &event3.labels));
    }

    // --- Backward compatibility: new() and with_timestamp() wrap in ValidatedMetricName ---

    #[test]
    fn new_wraps_name_in_validated_metric_name() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::new("up".to_string(), 1.0, labels).unwrap();
        // The name field should be a ValidatedMetricName, verifiable by cloning cheaply.
        let cloned = event.clone();
        assert!(Arc::ptr_eq(event.name.arc(), cloned.name.arc()));
    }

    #[test]
    fn new_wraps_labels_in_arc() {
        let labels = Labels::from_pairs(&[("k", "v")]).unwrap();
        let event = MetricEvent::new("up".to_string(), 1.0, labels).unwrap();
        let cloned = event.clone();
        assert!(Arc::ptr_eq(&event.labels, &cloned.labels));
    }

    #[test]
    fn with_timestamp_wraps_name_in_validated_metric_name() {
        let labels = Labels::from_pairs(&[]).unwrap();
        let ts = UNIX_EPOCH + Duration::from_secs(1);
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();
        let cloned = event.clone();
        assert!(Arc::ptr_eq(event.name.arc(), cloned.name.arc()));
    }

    #[test]
    fn with_timestamp_wraps_labels_in_arc() {
        let labels = Labels::from_pairs(&[("k", "v")]).unwrap();
        let ts = UNIX_EPOCH + Duration::from_secs(1);
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();
        let cloned = event.clone();
        assert!(Arc::ptr_eq(&event.labels, &cloned.labels));
    }
}
