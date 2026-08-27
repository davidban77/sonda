//! Capture a signal out of a Prometheus-compatible TSDB and replay it exactly.
//!
//! Fetches a PromQL range query, resamples it onto the requested grid, and
//! hands back what the CSV writer turns into a `csv_replay` scenario. Reached
//! from `sonda new --from-prometheus`.
//!
//! **Replay-only:** no classifier, no generator inference, no `--fit`. The case
//! this closes is alert regression, where the alert must fire on what actually
//! happened. Pattern fitting stays in `--from <csv>`, which promises a guess.
//!
//! Pure and feature-free except [`tsdb`], which is behind `http`.

#[cfg(feature = "http")]
pub mod tsdb;

pub mod csv_out;
pub mod normalize;
pub mod response;
#[cfg(feature = "config")]
pub mod yaml_out;

use std::collections::BTreeMap;

/// One time series as returned by a range query, before any grid alignment.
///
/// `labels` keeps `__name__` when the query preserved it — the CSV writer needs
/// it to name the column, and callers must not assume it is there, since
/// aggregations like `sum by (job)` drop it. Samples are
/// `(unix_seconds, value)` in ascending time order, as the TSDB reported them.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedSeries {
    /// The series' label set, `__name__` included when the query kept it.
    pub labels: BTreeMap<String, String>,
    /// `(unix_seconds, value)` pairs in ascending time order.
    pub samples: Vec<(f64, f64)>,
}

impl FetchedSeries {
    /// The metric name, if the query preserved `__name__`.
    pub fn metric_name(&self) -> Option<&str> {
        self.labels.get("__name__").map(String::as_str)
    }

    /// The label set without `__name__`.
    ///
    /// These are the labels that belong in the emitted column header's
    /// `{...}` block alongside the name.
    pub fn labels_without_name(&self) -> impl Iterator<Item = (&str, &str)> {
        self.labels
            .iter()
            .filter(|(k, _)| k.as_str() != "__name__")
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }
}
