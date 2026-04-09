//! Scenario metadata types for the built-in scenario catalog.
//!
//! This module defines the [`BuiltinScenario`] struct used by the CLI to
//! represent scenario entries discovered from the filesystem. Scenario YAML
//! files live outside the binary as standalone files in `scenarios/` at the
//! repository root, discovered via a search path.
//!
//! The CLI crate (`sonda`) owns the discovery logic (search path construction,
//! directory scanning, metadata probing). This module provides only the shared
//! data type.

/// A scenario entry discovered from the filesystem.
///
/// Populated by reading metadata fields (`scenario_name`, `category`,
/// `signal_type`, `description`) from the YAML file header. The full YAML
/// content is loaded lazily when the scenario is actually run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinScenario {
    /// Kebab-case identifier (e.g. `"cpu-spike"`).
    pub name: String,
    /// Broad grouping: `"infrastructure"`, `"network"`, `"application"`,
    /// or `"observability"`.
    pub category: String,
    /// The signal type this scenario produces: `"metrics"`, `"logs"`,
    /// `"multi"`, `"histogram"`, or `"summary"`.
    pub signal_type: String,
    /// One-line human-readable description for list display.
    pub description: String,
    /// Absolute path to the YAML file on disk.
    pub source_path: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Contract tests ---------------------------------------------------------

    #[test]
    fn builtin_scenario_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BuiltinScenario>();
    }

    #[test]
    fn builtin_scenario_clone_produces_equal_value() {
        let scenario = BuiltinScenario {
            name: "cpu-spike".to_string(),
            category: "infrastructure".to_string(),
            signal_type: "metrics".to_string(),
            description: "Test scenario".to_string(),
            source_path: std::path::PathBuf::from("/tmp/cpu-spike.yaml"),
        };
        let cloned = scenario.clone();
        assert_eq!(scenario, cloned);
    }

    #[test]
    fn builtin_scenario_debug_output_is_non_empty() {
        let scenario = BuiltinScenario {
            name: "test".to_string(),
            category: "test".to_string(),
            signal_type: "metrics".to_string(),
            description: "test".to_string(),
            source_path: std::path::PathBuf::from("/tmp/test.yaml"),
        };
        let debug = format!("{:?}", scenario);
        assert!(!debug.is_empty());
    }
}
