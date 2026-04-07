//! Pre-built scenario catalog embedded in the binary.
//!
//! This module provides a curated library of YAML scenario patterns that
//! are compiled into the binary via [`include_str!`]. Users can discover
//! available scenarios with [`list`], look them up by name with [`get`],
//! and retrieve raw YAML with [`get_yaml`].
//!
//! All built-in scenarios use `stdout` as their sink and have a finite
//! `duration`, so they work out of the box with zero configuration.
//!
//! # Examples
//!
//! ```
//! use sonda_core::scenarios;
//!
//! // List all available scenarios
//! let all = scenarios::list();
//! assert!(!all.is_empty());
//!
//! // Look up a specific scenario by name
//! let cpu = scenarios::get("cpu-spike");
//! assert!(cpu.is_some());
//!
//! // Get raw YAML for a scenario
//! let yaml = scenarios::get_yaml("cpu-spike");
//! assert!(yaml.is_some());
//! ```

/// A pre-built scenario definition embedded in the binary.
///
/// All fields are `&'static str` because the data is compiled in via
/// [`include_str!`]. The `yaml` field contains the full YAML content
/// that can be parsed into the appropriate config type based on
/// `signal_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinScenario {
    /// Kebab-case identifier (e.g. `"cpu-spike"`).
    pub name: &'static str,
    /// Broad grouping: `"infrastructure"`, `"network"`, `"application"`,
    /// or `"observability"`.
    pub category: &'static str,
    /// The signal type this scenario produces: `"metrics"`, `"logs"`,
    /// `"multi"`, or `"histogram"`.
    pub signal_type: &'static str,
    /// One-line human-readable description for list display.
    pub description: &'static str,
    /// The full embedded YAML content.
    pub yaml: &'static str,
}

/// The complete catalog of built-in scenarios.
///
/// This is a static array so there are zero heap allocations. The catalog
/// is small enough (~11 entries) that linear scan is the right choice
/// over a `HashMap`.
static CATALOG: &[BuiltinScenario] = &[
    BuiltinScenario {
        name: "cpu-spike",
        category: "infrastructure",
        signal_type: "metrics",
        description: "Periodic CPU usage spikes above threshold",
        yaml: include_str!("../../scenarios/cpu-spike.yaml"),
    },
    BuiltinScenario {
        name: "memory-leak",
        category: "infrastructure",
        signal_type: "metrics",
        description: "Monotonically growing memory usage (sawtooth)",
        yaml: include_str!("../../scenarios/memory-leak.yaml"),
    },
    BuiltinScenario {
        name: "disk-fill",
        category: "infrastructure",
        signal_type: "metrics",
        description: "Constant-rate disk consumption (step counter)",
        yaml: include_str!("../../scenarios/disk-fill.yaml"),
    },
    BuiltinScenario {
        name: "interface-flap",
        category: "network",
        signal_type: "multi",
        description: "Network interface toggling up/down with traffic shifts",
        yaml: include_str!("../../scenarios/interface-flap.yaml"),
    },
    BuiltinScenario {
        name: "latency-degradation",
        category: "application",
        signal_type: "metrics",
        description: "Growing response latency with jitter (sawtooth)",
        yaml: include_str!("../../scenarios/latency-degradation.yaml"),
    },
    BuiltinScenario {
        name: "error-rate-spike",
        category: "application",
        signal_type: "metrics",
        description: "Periodic HTTP error rate bursts",
        yaml: include_str!("../../scenarios/error-rate-spike.yaml"),
    },
    BuiltinScenario {
        name: "cardinality-explosion",
        category: "observability",
        signal_type: "metrics",
        description: "Pod label cardinality explosion with spike windows",
        yaml: include_str!("../../scenarios/cardinality-explosion.yaml"),
    },
    BuiltinScenario {
        name: "log-storm",
        category: "application",
        signal_type: "logs",
        description: "Error-level log burst with template generation",
        yaml: include_str!("../../scenarios/log-storm.yaml"),
    },
    BuiltinScenario {
        name: "steady-state",
        category: "infrastructure",
        signal_type: "metrics",
        description: "Normal oscillating baseline (sine + jitter)",
        yaml: include_str!("../../scenarios/steady-state.yaml"),
    },
    BuiltinScenario {
        name: "network-link-failure",
        category: "network",
        signal_type: "multi",
        description: "Link down with traffic shift to backup path",
        yaml: include_str!("../../scenarios/network-link-failure.yaml"),
    },
    BuiltinScenario {
        name: "histogram-latency",
        category: "application",
        signal_type: "histogram",
        description: "Request latency histogram (normal distribution)",
        yaml: include_str!("../../scenarios/histogram-latency.yaml"),
    },
];

/// Return the full catalog of built-in scenarios.
///
/// The returned slice is `&'static` — no allocation, no copying.
pub fn list() -> &'static [BuiltinScenario] {
    CATALOG
}

/// Look up a built-in scenario by its kebab-case name.
///
/// Returns `None` if no scenario with that name exists.
pub fn get(name: &str) -> Option<&'static BuiltinScenario> {
    CATALOG.iter().find(|s| s.name == name)
}

/// Convenience function to get the raw YAML for a built-in scenario.
///
/// Equivalent to `get(name).map(|s| s.yaml)`.
pub fn get_yaml(name: &str) -> Option<&'static str> {
    get(name).map(|s| s.yaml)
}

/// Return all built-in scenarios in a given category.
///
/// The category match is case-sensitive. Returns an empty `Vec` if no
/// scenarios belong to the requested category.
pub fn list_by_category(category: &str) -> Vec<&'static BuiltinScenario> {
    CATALOG.iter().filter(|s| s.category == category).collect()
}

/// Return a formatted list of all available scenario names.
///
/// Useful for error messages that want to hint at valid names.
pub fn available_names() -> Vec<&'static str> {
    CATALOG.iter().map(|s| s.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Catalog structure tests ------------------------------------------------

    #[test]
    fn catalog_is_not_empty() {
        assert!(
            !list().is_empty(),
            "built-in catalog must contain at least one scenario"
        );
    }

    #[test]
    fn all_names_are_unique() {
        let names: Vec<&str> = list().iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            names.len(),
            sorted.len(),
            "duplicate scenario names found in catalog"
        );
    }

    #[test]
    fn all_names_are_kebab_case() {
        for scenario in list() {
            assert!(
                scenario
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "scenario name {:?} must be kebab-case (lowercase + hyphens)",
                scenario.name
            );
            assert!(!scenario.name.is_empty(), "scenario name must not be empty");
        }
    }

    #[test]
    fn all_categories_are_known() {
        let known = ["infrastructure", "network", "application", "observability"];
        for scenario in list() {
            assert!(
                known.contains(&scenario.category),
                "scenario {:?} has unknown category {:?}; expected one of {:?}",
                scenario.name,
                scenario.category,
                known
            );
        }
    }

    #[test]
    fn all_signal_types_are_known() {
        let known = ["metrics", "logs", "multi", "histogram", "summary"];
        for scenario in list() {
            assert!(
                known.contains(&scenario.signal_type),
                "scenario {:?} has unknown signal_type {:?}; expected one of {:?}",
                scenario.name,
                scenario.signal_type,
                known
            );
        }
    }

    #[test]
    fn all_descriptions_are_non_empty() {
        for scenario in list() {
            assert!(
                !scenario.description.is_empty(),
                "scenario {:?} must have a non-empty description",
                scenario.name
            );
        }
    }

    #[test]
    fn all_yamls_are_non_empty() {
        for scenario in list() {
            assert!(
                !scenario.yaml.is_empty(),
                "scenario {:?} must have non-empty YAML",
                scenario.name
            );
        }
    }

    // ---- YAML parsing tests (require `config` feature) --------------------------

    #[cfg(feature = "config")]
    #[test]
    fn all_metrics_yamls_parse_as_scenario_config() {
        use crate::config::ScenarioConfig;

        for scenario in list().iter().filter(|s| s.signal_type == "metrics") {
            let result = serde_yaml_ng::from_str::<ScenarioConfig>(scenario.yaml);
            assert!(
                result.is_ok(),
                "metrics scenario {:?} failed to parse: {:?}",
                scenario.name,
                result.err()
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_logs_yamls_parse_as_log_scenario_config() {
        use crate::config::LogScenarioConfig;

        for scenario in list().iter().filter(|s| s.signal_type == "logs") {
            let result = serde_yaml_ng::from_str::<LogScenarioConfig>(scenario.yaml);
            assert!(
                result.is_ok(),
                "logs scenario {:?} failed to parse: {:?}",
                scenario.name,
                result.err()
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_multi_yamls_parse_as_multi_scenario_config() {
        use crate::config::MultiScenarioConfig;

        for scenario in list().iter().filter(|s| s.signal_type == "multi") {
            let result = serde_yaml_ng::from_str::<MultiScenarioConfig>(scenario.yaml);
            assert!(
                result.is_ok(),
                "multi scenario {:?} failed to parse: {:?}",
                scenario.name,
                result.err()
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_histogram_yamls_parse_as_histogram_scenario_config() {
        use crate::config::HistogramScenarioConfig;

        for scenario in list().iter().filter(|s| s.signal_type == "histogram") {
            let result = serde_yaml_ng::from_str::<HistogramScenarioConfig>(scenario.yaml);
            assert!(
                result.is_ok(),
                "histogram scenario {:?} failed to parse: {:?}",
                scenario.name,
                result.err()
            );
        }
    }

    // ---- Convention tests: stdout sink and finite duration -----------------------

    #[cfg(feature = "config")]
    #[test]
    fn all_scenarios_use_stdout_sink() {
        // Verify by checking that each YAML contains "type: stdout" in its
        // sink section. This is a textual check since multi-scenario configs
        // have multiple sink entries.
        for scenario in list() {
            assert!(
                scenario.yaml.contains("type: stdout"),
                "scenario {:?} must use stdout sink for zero-config usability",
                scenario.name
            );
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn all_scenarios_have_finite_duration() {
        // Verify that each YAML contains a "duration:" field.
        for scenario in list() {
            assert!(
                scenario.yaml.contains("duration:"),
                "scenario {:?} must have a finite duration for self-termination",
                scenario.name
            );
        }
    }

    // ---- Lookup function tests --------------------------------------------------

    #[test]
    fn get_existing_scenario_returns_some() {
        let scenario = get("cpu-spike");
        assert!(scenario.is_some(), "cpu-spike must exist in catalog");
        let s = scenario.expect("checked above");
        assert_eq!(s.name, "cpu-spike");
        assert_eq!(s.category, "infrastructure");
        assert_eq!(s.signal_type, "metrics");
    }

    #[test]
    fn get_nonexistent_scenario_returns_none() {
        assert!(
            get("nonexistent-scenario").is_none(),
            "nonexistent scenario must return None"
        );
    }

    #[test]
    fn get_yaml_returns_yaml_content() {
        let yaml = get_yaml("cpu-spike");
        assert!(yaml.is_some());
        let content = yaml.expect("checked above");
        assert!(content.contains("name:"), "YAML must contain a name field");
    }

    #[test]
    fn get_yaml_nonexistent_returns_none() {
        assert!(get_yaml("does-not-exist").is_none());
    }

    #[test]
    fn list_by_category_infrastructure() {
        let infra = list_by_category("infrastructure");
        assert!(
            !infra.is_empty(),
            "infrastructure category must have at least one scenario"
        );
        for s in &infra {
            assert_eq!(s.category, "infrastructure");
        }
    }

    #[test]
    fn list_by_category_network() {
        let network = list_by_category("network");
        assert!(
            !network.is_empty(),
            "network category must have at least one scenario"
        );
        for s in &network {
            assert_eq!(s.category, "network");
        }
    }

    #[test]
    fn list_by_category_application() {
        let app = list_by_category("application");
        assert!(
            !app.is_empty(),
            "application category must have at least one scenario"
        );
        for s in &app {
            assert_eq!(s.category, "application");
        }
    }

    #[test]
    fn list_by_category_unknown_returns_empty() {
        let unknown = list_by_category("nonexistent-category");
        assert!(
            unknown.is_empty(),
            "unknown category must return empty list"
        );
    }

    #[test]
    fn available_names_matches_catalog_count() {
        let names = available_names();
        assert_eq!(
            names.len(),
            list().len(),
            "available_names must return one name per catalog entry"
        );
    }

    #[test]
    fn available_names_contains_cpu_spike() {
        let names = available_names();
        assert!(
            names.contains(&"cpu-spike"),
            "available_names must include cpu-spike"
        );
    }

    // ---- Contract tests ---------------------------------------------------------

    #[test]
    fn builtin_scenario_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BuiltinScenario>();
    }
}
