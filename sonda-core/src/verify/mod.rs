//! Alert expectation verification — the engine behind `sonda test`.
//!
//! A scenario file may carry a top-level `expect:` block declaring which
//! alerts the generated signals should trigger, and how quickly:
//!
//! ```yaml
//! expect:
//!   alerts:
//!     - alert: HighCpuUsage
//!       firing_within: 2m
//!       resolves_within: 1m
//!       labels: { severity: critical }
//! ```
//!
//! The block is pure metadata to the compiler and runtime — scenarios run
//! identically with or without it. [`parse_expectations`] extracts it, and
//! the [`prometheus`] client (feature `http`) polls a Prometheus-compatible
//! API's `ALERTS` metric to check each expectation.

pub mod evaluator;
#[cfg(feature = "http")]
pub mod prometheus;

use std::collections::BTreeMap;
use std::time::Duration;

use crate::SondaError;

/// Observed state of one alert at a single poll.
///
/// Produced by acquisition (the [`prometheus`] client today, Alertmanager
/// sampling in a later phase) and consumed by the [`evaluator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertState {
    /// No alert series matched the expectation's selector.
    Inactive,
    /// Matched series exist, but none is firing.
    Pending,
    /// At least one matched series is firing.
    Firing,
}

/// Top-level `expect:` block of a scenario file.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
pub struct ExpectConfig {
    /// Alert expectations, checked in order. Must not be empty.
    pub alerts: Vec<AlertExpectation>,
}

/// One alert the scenario is expected to trigger.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
pub struct AlertExpectation {
    /// Alert rule name — matched against the `alertname` label of the
    /// `ALERTS` metric.
    pub alert: String,
    /// Additional label matchers the firing alert must carry.
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub labels: Option<BTreeMap<String, String>>,
    /// How long after scenario start the alert must reach `firing`
    /// (e.g. `"2m"`).
    pub firing_within: String,
    /// How long after scenario end the alert must stop firing. `None`
    /// skips the resolution check.
    #[cfg_attr(
        feature = "config",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub resolves_within: Option<String>,
}

impl AlertExpectation {
    /// Parsed `firing_within` deadline.
    pub fn firing_within(&self) -> Result<Duration, SondaError> {
        crate::config::validate::parse_duration(&self.firing_within)
    }

    /// Parsed `resolves_within` deadline, when present.
    pub fn resolves_within(&self) -> Result<Option<Duration>, SondaError> {
        self.resolves_within
            .as_deref()
            .map(crate::config::validate::parse_duration)
            .transpose()
    }
}

/// Label values on a matched alert series that the scenario never emitted.
///
/// The `ALERTS` metric is global: an expectation can match an alert another
/// series caused. This compares a matched series' labels against the label
/// values the scenario actually emits — for every label key the scenario
/// uses, a differing value on the series means the alert (at least partly)
/// came from outside the scenario. Callers surface these as warnings so an
/// under-scoped expectation is loud instead of silently green. Label keys
/// the scenario never sets (e.g. `alertname`, `alertstate`, rule labels
/// like `severity`) are not compared — the scenario has no opinion on them.
pub fn foreign_label_values(
    series: &BTreeMap<String, String>,
    emitted: &BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Vec<(String, String)> {
    series
        .iter()
        .filter(|(key, value)| {
            emitted
                .get(*key)
                .is_some_and(|values| !values.contains(*value))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Extract and validate the `expect:` block from scenario YAML.
///
/// Returns `Ok(None)` when the file has no `expect:` block. The probe is
/// deliberately lenient about every other key — full scenario validation is
/// the compiler's job, and callers run both.
///
/// # Errors
///
/// Returns [`SondaError::Config`] when the block is present but malformed:
/// unknown fields, an empty alert list, or invalid duration strings.
#[cfg(feature = "config")]
pub fn parse_expectations(yaml: &str) -> Result<Option<ExpectConfig>, SondaError> {
    use crate::ConfigError;

    #[derive(serde::Deserialize)]
    struct ExpectProbe {
        #[serde(default)]
        expect: Option<serde_yaml_ng::Value>,
    }

    let probe: ExpectProbe = serde_yaml_ng::from_str(yaml)
        .map_err(|e| SondaError::Config(ConfigError::invalid(format!("invalid YAML: {e}"))))?;
    let Some(raw) = probe.expect else {
        return Ok(None);
    };
    let config: ExpectConfig = serde_yaml_ng::from_value(raw).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!("invalid expect block: {e}")))
    })?;

    if config.alerts.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "expect.alerts must list at least one alert",
        )));
    }
    for expectation in &config.alerts {
        if expectation.alert.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "expect.alerts[].alert must not be empty",
            )));
        }
        expectation.firing_within()?;
        expectation.resolves_within()?;
    }
    Ok(Some(config))
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn emitted(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs
            .iter()
            .map(|(k, vs)| {
                (
                    k.to_string(),
                    vs.iter().map(|v| v.to_string()).collect::<BTreeSet<_>>(),
                )
            })
            .collect()
    }

    fn series(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn matching_values_produce_no_warnings() {
        let foreign = foreign_label_values(
            &series(&[("host", "sonda-test"), ("alertname", "HighCpuUsage")]),
            &emitted(&[("host", &["sonda-test"])]),
        );
        assert!(foreign.is_empty());
    }

    #[test]
    fn differing_value_on_an_emitted_key_is_foreign() {
        // The #527 review's exact case: expectation matched an alert from
        // host=ci-test-node while the scenario only emitted host=sonda-test.
        let foreign = foreign_label_values(
            &series(&[("host", "ci-test-node"), ("severity", "critical")]),
            &emitted(&[("host", &["sonda-test"]), ("service", &["payments"])]),
        );
        assert_eq!(
            foreign,
            vec![("host".to_string(), "ci-test-node".to_string())]
        );
    }

    #[test]
    fn keys_the_scenario_never_sets_are_ignored() {
        // alertname / alertstate / rule labels are not scenario labels.
        let foreign = foreign_label_values(
            &series(&[("alertname", "HighCpuUsage"), ("severity", "critical")]),
            &emitted(&[("host", &["sonda-test"])]),
        );
        assert!(foreign.is_empty());
    }

    #[test]
    fn any_emitted_value_for_the_key_matches() {
        // Multi-entry scenarios can emit several values for one key.
        let foreign = foreign_label_values(
            &series(&[("host", "b")]),
            &emitted(&[("host", &["a", "b"])]),
        );
        assert!(foreign.is_empty());
    }
}

#[cfg(all(test, feature = "config"))]
mod tests {
    use super::*;

    const FULL: &str = "
version: 2
kind: runnable
scenarios: []
expect:
  alerts:
    - alert: HighCpuUsage
      firing_within: 2m
      resolves_within: 1m
      labels: { severity: critical }
    - alert: ElevatedCpuUsage
      firing_within: 90s
";

    #[test]
    fn parses_full_expect_block() {
        let config = parse_expectations(FULL)
            .expect("parse ok")
            .expect("present");
        assert_eq!(config.alerts.len(), 2);
        assert_eq!(config.alerts[0].alert, "HighCpuUsage");
        assert_eq!(
            config.alerts[0].firing_within().expect("duration"),
            Duration::from_secs(120)
        );
        assert_eq!(
            config.alerts[0].resolves_within().expect("duration"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(config.alerts[1].resolves_within().expect("none"), None);
        let labels = config.alerts[0].labels.as_ref().expect("labels");
        assert_eq!(labels.get("severity").map(String::as_str), Some("critical"));
    }

    #[test]
    fn absent_block_is_none() {
        let yaml = "version: 2\nkind: runnable\nscenarios: []\n";
        assert!(parse_expectations(yaml).expect("parse ok").is_none());
    }

    #[test]
    fn empty_alert_list_is_rejected() {
        let yaml = "version: 2\nexpect:\n  alerts: []\n";
        let err = parse_expectations(yaml).expect_err("must fail");
        assert!(err.to_string().contains("at least one alert"));
    }

    #[test]
    fn invalid_duration_is_rejected() {
        let yaml = "
expect:
  alerts:
    - alert: X
      firing_within: banana
";
        assert!(parse_expectations(yaml).is_err());
    }

    #[test]
    fn unknown_expectation_field_is_rejected() {
        let yaml = "
expect:
  alerts:
    - alert: X
      firing_within: 1m
      fires_within: 1m
";
        let err = parse_expectations(yaml).expect_err("must fail");
        assert!(err.to_string().contains("invalid expect block"));
    }
}
