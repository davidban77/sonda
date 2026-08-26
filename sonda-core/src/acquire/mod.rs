//! Capture a signal out of a Prometheus-compatible TSDB and replay it exactly.
//!
//! It fetches a PromQL range query, resamples the result onto the requested
//! step grid, and hands back data the CSV writer turns into a `csv_replay`
//! scenario.
//!
//! No CLI surface reaches this yet — `sonda new` offers `--template` and
//! `--from <FILE>` and nothing else. This module is the acquisition half of an
//! importer still being built; do not name a flag here until one exists.
//!
//! # Replay-only — there is no fitting in this path
//!
//! Nothing here inspects the *shape* of a signal. No classifier, no generator
//! inference, no `--fit`. The recorded values are written out verbatim and
//! replayed verbatim. That is a deliberate product decision, not an omission:
//! the use case this closes is alert regression, where the alert must fire on
//! what actually happened, so an exact replay is strictly better than an
//! idealised generator. Pattern fitting stays in the `--from <csv>` starter
//! wizard, where its promise level is "a guess to edit".
//!
//! # Layering
//!
//! Everything in this module is pure and feature-free: parsing a matrix
//! response, resampling onto a grid, and writing CSV are all plain functions
//! of their arguments, unit-tested without a network. Only [`tsdb`], which
//! owns the HTTP client, is behind the `http` feature.

#[cfg(feature = "http")]
pub mod tsdb;

pub mod csv_out;
pub mod normalize;
pub mod response;
#[cfg(feature = "config")]
pub mod yaml_out;

use std::collections::BTreeMap;

/// One time series as returned by a range query.
///
/// `labels` is the series' full label set with `__name__` still present when
/// the query preserved it — the CSV writer needs it to name the column, and
/// callers must not assume it exists (aggregations like `sum by (job)` drop
/// it). Samples are `(unix_seconds, value)` in ascending time order, exactly
/// as the TSDB reported them: no grid alignment has happened yet.
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
