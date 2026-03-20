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
fn is_valid_metric_name(s: &str) -> bool {
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
