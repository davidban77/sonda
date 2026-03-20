//! Canonical metric event representation.
//!
//! Format-agnostic — encoding to Prometheus, Influx, or JSON is the encoder's concern.

use std::collections::BTreeMap;
use std::time::SystemTime;

/// An ordered, deduplicated set of string label key-value pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct Labels {
    inner: BTreeMap<String, String>,
}

impl Labels {
    /// Create a new label set from key-value pairs.
    /// Duplicate keys are resolved by last-write-wins.
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        let inner = pairs.into_iter().collect();
        Self { inner }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.inner.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// A single timestamped metric sample.
#[derive(Debug, Clone)]
pub struct MetricEvent {
    pub name: String,
    pub value: f64,
    pub labels: Labels,
    pub timestamp: SystemTime,
}

impl MetricEvent {
    pub fn new(name: String, value: f64, labels: Labels) -> Self {
        Self {
            name,
            value,
            labels,
            timestamp: SystemTime::now(),
        }
    }
}
