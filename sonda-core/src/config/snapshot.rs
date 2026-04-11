//! Deterministic JSON snapshot serializer for compiled scenario entries.
//!
//! This module provides [`snapshot_entries`] and [`snapshot_prepared_entries`],
//! which serialize scenario configurations into a stable, machine-readable JSON
//! format suitable for golden-file test comparisons.
//!
//! The snapshot captures every field that affects runtime behavior: generator
//! parameters, labels, schedule windows, encoder settings, sink configuration,
//! phase offsets, and clock groups.
//!
//! **Determinism guarantee**: all map keys (including `HashMap<String, String>`
//! labels) are sorted lexicographically by serializing through an intermediate
//! `serde_json::Value` and recursively sorting all object keys before
//! pretty-printing. This ensures identical input always produces identical
//! output regardless of `HashMap` iteration order.
//!
//! This is **test infrastructure** for the v2 refactor, not a user-facing
//! feature. The JSON format is an internal contract between the snapshot
//! serializer and the golden-file tests.

use crate::config::ScenarioEntry;
use crate::schedule::launch::PreparedEntry;

/// Serialize compiled scenario entries to a deterministic JSON snapshot.
///
/// Produces a pretty-printed JSON array where each element represents one
/// [`ScenarioEntry`]. The output is deterministic: identical input always
/// produces identical output, making it suitable for golden-file comparisons.
///
/// Map keys (e.g., labels) are sorted lexicographically via a post-processing
/// pass over the intermediate `serde_json::Value` tree. Floating-point values
/// preserve full `f64` precision.
///
/// # Panics
///
/// Panics if serialization fails, which should never happen for well-formed
/// config types. This function is intended for test infrastructure where a
/// serialization failure indicates a bug in the type definitions.
#[cfg(feature = "config")]
pub fn snapshot_entries(entries: &[ScenarioEntry]) -> String {
    let value = serde_json::to_value(entries)
        .expect("snapshot serialization must not fail for well-formed config types");
    let sorted = sort_json_keys(value);
    serde_json::to_string_pretty(&sorted)
        .expect("pretty-printing a serde_json::Value must not fail")
}

/// Serialize prepared entries (post-validation, post-expansion) to a
/// deterministic JSON snapshot.
///
/// Each element includes the scenario entry and its resolved `start_delay`
/// (in milliseconds, or `null` when absent). This captures the true compile
/// boundary — the output of [`crate::schedule::launch::prepare_entries`].
///
/// # Panics
///
/// Panics if serialization fails, which should never happen for well-formed
/// config types.
#[cfg(feature = "config")]
pub fn snapshot_prepared_entries(entries: &[PreparedEntry]) -> String {
    let snapshots: Vec<PreparedSnapshot<'_>> = entries
        .iter()
        .map(|p| PreparedSnapshot {
            entry: &p.entry,
            start_delay_ms: p.start_delay.map(|d| d.as_millis() as u64),
        })
        .collect();
    let value = serde_json::to_value(&snapshots)
        .expect("snapshot serialization must not fail for well-formed config types");
    let sorted = sort_json_keys(value);
    serde_json::to_string_pretty(&sorted)
        .expect("pretty-printing a serde_json::Value must not fail")
}

/// Intermediate representation for prepared entry snapshots.
#[cfg(feature = "config")]
#[derive(serde::Serialize)]
struct PreparedSnapshot<'a> {
    /// The scenario entry configuration.
    #[serde(flatten)]
    entry: &'a ScenarioEntry,
    /// Resolved start delay in milliseconds, or `null` when absent.
    start_delay_ms: Option<u64>,
}

/// Recursively sort all object keys in a JSON value tree.
///
/// `serde_json::Map` preserves insertion order, which for `HashMap` sources is
/// non-deterministic. This function walks the tree and rebuilds every object
/// with keys in sorted order, ensuring stable output for golden-file
/// comparisons.
fn sort_json_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (k, v) in entries {
                sorted.insert(k, sort_json_keys(v));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_json_keys).collect())
        }
        other => other,
    }
}

/// Compare a snapshot string against a golden file, with optional update mode.
///
/// When the `UPDATE_SNAPSHOTS` environment variable is set to `"1"`, the golden
/// file is overwritten with the actual snapshot. Otherwise, the function returns
/// `Ok(())` if the snapshots match, or `Err` with a diff message if they diverge.
///
/// # Parameters
///
/// * `actual` — the freshly generated snapshot string.
/// * `golden_path` — path to the expected golden file.
///
/// # Returns
///
/// `Ok(())` when the snapshot matches (or was updated). `Err(String)` with a
/// diagnostic message when the snapshots diverge.
pub fn assert_or_update_snapshot(
    actual: &str,
    golden_path: &std::path::Path,
) -> Result<(), String> {
    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        if let Some(parent) = golden_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "failed to create golden file directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }
        std::fs::write(golden_path, actual).map_err(|e| {
            format!(
                "failed to write golden file {}: {}",
                golden_path.display(),
                e
            )
        })?;
        return Ok(());
    }

    let expected = std::fs::read_to_string(golden_path).map_err(|e| {
        format!(
            "failed to read golden file {} (run with UPDATE_SNAPSHOTS=1 to create it): {}",
            golden_path.display(),
            e
        )
    })?;

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "snapshot mismatch for {}\n\n--- expected ---\n{}\n--- actual ---\n{}\n\n\
             Run with UPDATE_SNAPSHOTS=1 to update the golden file.",
            golden_path.display(),
            expected,
            actual,
        ))
    }
}

#[cfg(all(test, feature = "config"))]
mod tests {
    use super::*;
    use crate::config::{BaseScheduleConfig, ScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    use crate::sink::SinkConfig;

    fn make_constant_entry(name: &str, value: f64) -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: name.to_string(),
                rate: 1.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })
    }

    #[test]
    fn snapshot_single_entry_is_deterministic() {
        let entry = make_constant_entry("cpu_usage", 42.0);
        let snap1 = snapshot_entries(&[entry.clone()]);
        let snap2 = snapshot_entries(&[entry]);
        assert_eq!(
            snap1, snap2,
            "identical input must produce identical output"
        );
    }

    #[test]
    fn snapshot_contains_generator_fields() {
        let entry = make_constant_entry("test_metric", 99.5);
        let snap = snapshot_entries(&[entry]);
        assert!(
            snap.contains("\"value\": 99.5"),
            "snapshot must contain the generator value"
        );
        assert!(
            snap.contains("\"type\": \"constant\""),
            "snapshot must contain the generator type tag"
        );
    }

    #[test]
    fn snapshot_contains_scenario_name() {
        let entry = make_constant_entry("my_metric", 1.0);
        let snap = snapshot_entries(&[entry]);
        assert!(
            snap.contains("\"name\": \"my_metric\""),
            "snapshot must contain the scenario name"
        );
    }

    #[test]
    fn snapshot_multiple_entries_produces_array() {
        let entries = vec![
            make_constant_entry("metric_a", 1.0),
            make_constant_entry("metric_b", 2.0),
        ];
        let snap = snapshot_entries(&entries);
        let parsed: serde_json::Value =
            serde_json::from_str(&snap).expect("snapshot must be valid JSON");
        assert!(parsed.is_array(), "snapshot must be a JSON array");
        assert_eq!(
            parsed.as_array().unwrap().len(),
            2,
            "array must have two elements"
        );
    }

    #[test]
    fn snapshot_empty_entries_produces_empty_array() {
        let snap = snapshot_entries(&[]);
        assert_eq!(snap, "[]", "empty input must produce empty JSON array");
    }

    #[test]
    fn snapshot_labels_are_sorted() {
        let mut labels = std::collections::HashMap::new();
        labels.insert("zone".to_string(), "eu1".to_string());
        labels.insert("hostname".to_string(), "t0-a1".to_string());
        labels.insert("arch".to_string(), "x86".to_string());

        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "labeled_metric".to_string(),
                rate: 1.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: Some(labels),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });

        let snap = snapshot_entries(&[entry]);
        let arch_pos = snap.find("\"arch\"").expect("must contain arch label");
        let hostname_pos = snap
            .find("\"hostname\"")
            .expect("must contain hostname label");
        let zone_pos = snap.find("\"zone\"").expect("must contain zone label");
        assert!(
            arch_pos < hostname_pos && hostname_pos < zone_pos,
            "labels must be sorted alphabetically: arch < hostname < zone"
        );
    }

    #[test]
    fn snapshot_prepared_entry_includes_start_delay() {
        let entry = make_constant_entry("delayed_metric", 1.0);
        let prepared = vec![PreparedEntry {
            entry,
            start_delay: Some(std::time::Duration::from_millis(5000)),
        }];
        let snap = snapshot_prepared_entries(&prepared);
        assert!(
            snap.contains("\"start_delay_ms\": 5000"),
            "snapshot must include resolved start_delay_ms"
        );
    }

    #[test]
    fn snapshot_prepared_entry_null_delay_when_absent() {
        let entry = make_constant_entry("immediate_metric", 1.0);
        let prepared = vec![PreparedEntry {
            entry,
            start_delay: None,
        }];
        let snap = snapshot_prepared_entries(&prepared);
        assert!(
            snap.contains("\"start_delay_ms\": null"),
            "snapshot must show null for absent start_delay"
        );
    }

    #[test]
    fn sort_json_keys_sorts_nested_objects() {
        let input = serde_json::json!({
            "z_key": 1,
            "a_key": {
                "nested_z": true,
                "nested_a": false
            }
        });
        let sorted = sort_json_keys(input);
        let output = serde_json::to_string(&sorted).unwrap();
        let a_pos = output.find("\"a_key\"").unwrap();
        let z_pos = output.find("\"z_key\"").unwrap();
        assert!(a_pos < z_pos, "a_key must come before z_key");

        let na_pos = output.find("\"nested_a\"").unwrap();
        let nz_pos = output.find("\"nested_z\"").unwrap();
        assert!(na_pos < nz_pos, "nested_a must come before nested_z");
    }

    #[test]
    fn assert_or_update_passes_on_match() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let golden = dir.path().join("test.json");
        std::fs::write(&golden, "test content").unwrap();

        let result = assert_or_update_snapshot("test content", &golden);
        assert!(result.is_ok(), "matching content must pass");
    }

    #[test]
    fn assert_or_update_fails_on_mismatch() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let golden = dir.path().join("test.json");
        std::fs::write(&golden, "expected content").unwrap();

        let result = assert_or_update_snapshot("actual content", &golden);
        assert!(result.is_err(), "mismatched content must fail");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("snapshot mismatch"),
            "error must mention mismatch"
        );
    }

    #[test]
    fn assert_or_update_fails_when_golden_missing() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let golden = dir.path().join("nonexistent.json");

        let result = assert_or_update_snapshot("some content", &golden);
        assert!(result.is_err(), "missing golden file must fail");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("UPDATE_SNAPSHOTS=1"),
            "error must hint at UPDATE_SNAPSHOTS"
        );
    }
}
