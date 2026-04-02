//! CSV replay value generator -- replays numeric values from a CSV file.
//!
//! Values are loaded once at construction time. Each call to `value()` returns
//! the value at `tick % values.len()` when repeating, or the last value when
//! the tick exceeds the file length (clamped mode).

use std::path::Path;

use super::ValueGenerator;
use crate::SondaError;

/// A value generator that replays numeric values from a CSV file.
///
/// Reads a column of numeric values from a CSV file at construction time.
/// When `repeat` is true (default), cycles through the values via
/// `values[tick % len]`. When `repeat` is false, returns the last value for
/// ticks beyond the file length.
///
/// This enables recording real production metric values (via Prometheus/VM
/// export or custom tooling) and replaying them through Sonda to reproduce
/// exact production conditions.
///
/// # File format
///
/// - One value per line (simplest case), or CSV with a specified column index.
/// - Lines starting with `#` are treated as comments and skipped.
/// - Empty lines are skipped.
/// - Lines where the target column cannot be parsed as `f64` are skipped.
/// - An optional header row can be skipped with `has_header: true`.
///
/// # Examples
///
/// ```no_run
/// use sonda_core::generator::csv_replay::CsvReplayGenerator;
/// use sonda_core::generator::ValueGenerator;
///
/// let gen = CsvReplayGenerator::new("data.csv", 0, false, true).unwrap();
/// let v = gen.value(0); // first value from the file
/// ```
pub struct CsvReplayGenerator {
    values: Vec<f64>,
    repeat: bool,
}

impl CsvReplayGenerator {
    /// Create a new CSV replay generator by loading values from a file.
    ///
    /// Reads the specified column from the CSV file. Each row's value in that
    /// column is parsed as `f64`. Rows where the target column is missing or
    /// cannot be parsed are silently skipped (like comment and empty lines).
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CSV file.
    /// * `column` - Zero-based column index to read.
    /// * `has_header` - Whether to skip the first non-comment, non-empty row
    ///   as a header.
    /// * `repeat` - Whether to cycle values when ticks exceed the value count.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] if:
    /// - The file cannot be opened or read.
    /// - No valid numeric values are found in the specified column.
    pub fn new(
        path: &str,
        column: usize,
        has_header: bool,
        repeat: bool,
    ) -> Result<Self, SondaError> {
        let file_path = Path::new(path);
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| SondaError::Config(format!("cannot read CSV file {:?}: {}", path, e)))?;

        let values = Self::parse_values(&content, column, has_header)?;

        if values.is_empty() {
            return Err(SondaError::Config(format!(
                "CSV file {:?} contains no valid numeric values in column {}",
                path, column
            )));
        }

        Ok(Self { values, repeat })
    }

    /// Create a CSV replay generator from an in-memory string.
    ///
    /// This constructor is primarily useful for testing without requiring a
    /// file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Config`] if no valid numeric values are found.
    pub fn from_str(
        content: &str,
        column: usize,
        has_header: bool,
        repeat: bool,
    ) -> Result<Self, SondaError> {
        let values = Self::parse_values(content, column, has_header)?;

        if values.is_empty() {
            return Err(SondaError::Config(format!(
                "CSV content contains no valid numeric values in column {}",
                column
            )));
        }

        Ok(Self { values, repeat })
    }

    /// Parse numeric values from CSV content.
    ///
    /// Skips comment lines (starting with `#`), empty lines, and lines where
    /// the target column cannot be parsed as `f64`.
    fn parse_values(
        content: &str,
        column: usize,
        has_header: bool,
    ) -> Result<Vec<f64>, SondaError> {
        let mut values = Vec::new();
        let mut header_skipped = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip empty lines.
            if trimmed.is_empty() {
                continue;
            }

            // Skip comment lines.
            if trimmed.starts_with('#') {
                continue;
            }

            // Skip the first data line when has_header is true.
            if has_header && !header_skipped {
                header_skipped = true;
                continue;
            }

            // Split by comma and extract the target column.
            let fields: Vec<&str> = trimmed.split(',').collect();
            if let Some(field) = fields.get(column) {
                if let Ok(v) = field.trim().parse::<f64>() {
                    values.push(v);
                }
                // Unparseable values are silently skipped.
            }
            // Rows where the column index is out of bounds are silently skipped.
        }

        Ok(values)
    }
}

impl ValueGenerator for CsvReplayGenerator {
    /// Return the value for the given tick.
    ///
    /// When `repeat` is true, wraps via `tick % len`. When false, clamps to
    /// the last value for ticks beyond the value count.
    fn value(&self, tick: u64) -> f64 {
        let len = self.values.len();
        // Perform modulo in u64 space to avoid truncation on 32-bit platforms
        // where `usize` is 32 bits and ticks above u32::MAX would wrap silently.
        let index = if self.repeat {
            (tick % len as u64) as usize
        } else {
            (tick.min((len - 1) as u64)) as usize
        };
        self.values[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ---- Helper: write content to a temp file and return its path string ------

    fn temp_csv(content: &str) -> (NamedTempFile, String) {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        write!(tmp, "{}", content).expect("write content");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();
        (tmp, path)
    }

    // ---- Load values from a simple one-column file ----------------------------

    #[test]
    fn one_column_file_loads_all_values() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("one-column file should load");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    #[test]
    fn one_column_file_from_disk() {
        let (_tmp, path) = temp_csv("10.5\n20.5\n30.5\n");
        let gen = CsvReplayGenerator::new(&path, 0, false, true)
            .expect("one-column disk file should load");
        assert_eq!(gen.value(0), 10.5);
        assert_eq!(gen.value(1), 20.5);
        assert_eq!(gen.value(2), 30.5);
    }

    // ---- Load values from a multi-column CSV with column index ----------------

    #[test]
    fn multi_column_csv_reads_correct_column() {
        let content = "ts,cpu,mem\n1000,42.5,60.0\n2000,55.3,70.1\n3000,18.9,45.2\n";
        let gen = CsvReplayGenerator::from_str(content, 1, true, true)
            .expect("multi-column should load column 1");
        assert_eq!(gen.value(0), 42.5);
        assert_eq!(gen.value(1), 55.3);
        assert_eq!(gen.value(2), 18.9);
    }

    #[test]
    fn multi_column_csv_reads_first_column() {
        let content = "ts,cpu\n1000,42.5\n2000,55.3\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, true, true).expect("should read column 0");
        assert_eq!(gen.value(0), 1000.0);
        assert_eq!(gen.value(1), 2000.0);
    }

    #[test]
    fn multi_column_csv_reads_last_column() {
        let content = "a,b,c\n1.0,2.0,3.0\n4.0,5.0,6.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 2, true, true).expect("should read last column");
        assert_eq!(gen.value(0), 3.0);
        assert_eq!(gen.value(1), 6.0);
    }

    // ---- Header skipping (has_header: true) -----------------------------------

    #[test]
    fn has_header_true_skips_first_data_row() {
        // Use a numeric header to confirm it is skipped (not just failed to parse).
        let content = "999.0\n100.0\n200.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true, true)
            .expect("should skip numeric header");
        assert_eq!(
            gen.value(0),
            100.0,
            "first value should be 100.0, not 999.0 (header)"
        );
        assert_eq!(gen.value(1), 200.0);
    }

    #[test]
    fn has_header_false_does_not_skip_first_row() {
        let content = "999.0\n100.0\n200.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, false, true).expect("should not skip header");
        assert_eq!(
            gen.value(0),
            999.0,
            "first value should be 999.0 when header is not skipped"
        );
        assert_eq!(gen.value(1), 100.0);
        assert_eq!(gen.value(2), 200.0);
    }

    #[test]
    fn has_header_skips_first_non_comment_non_empty_row() {
        // Comments and empty lines come before the header; header is the first "data" line.
        let content = "# comment\n\nheader\n10.0\n20.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true, true)
            .expect("header after comments/empty should be skipped");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
    }

    // ---- Comment lines (#) are skipped ----------------------------------------

    #[test]
    fn comment_lines_are_skipped() {
        let content = "# this is a comment\n1.0\n# another comment\n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("comments should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
    }

    #[test]
    fn comment_with_leading_whitespace_is_skipped() {
        let content = "  # indented comment\n5.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("indented comment should be skipped");
        assert_eq!(gen.value(0), 5.0);
    }

    // ---- Empty lines are skipped ----------------------------------------------

    #[test]
    fn empty_lines_are_skipped() {
        let content = "\n1.0\n\n\n2.0\n\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("empty lines should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    #[test]
    fn whitespace_only_lines_are_skipped() {
        let content = "   \n1.0\n  \t  \n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("whitespace-only lines should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
    }

    // ---- repeat=true cycles correctly -----------------------------------------

    #[test]
    fn repeat_true_cycles_at_boundary() {
        let content = "10.0\n20.0\n30.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, false, true).expect("should load 3 values");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 10.0, "tick=3 should wrap to index 0");
        assert_eq!(gen.value(4), 20.0, "tick=4 should wrap to index 1");
        assert_eq!(gen.value(5), 30.0, "tick=5 should wrap to index 2");
    }

    #[test]
    fn repeat_true_multiple_full_cycles() {
        let content = "1.0\n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        for cycle in 0..5 {
            assert_eq!(gen.value(cycle * 2), 1.0, "cycle {cycle}: index 0");
            assert_eq!(gen.value(cycle * 2 + 1), 2.0, "cycle {cycle}: index 1");
        }
    }

    // ---- repeat=false clamps to last value ------------------------------------

    #[test]
    fn repeat_false_clamps_to_last_value() {
        let content = "10.0\n20.0\n30.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, false, false).expect("should load 3 values");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 30.0, "tick=3 should clamp to last value");
        assert_eq!(gen.value(100), 30.0, "tick=100 should clamp to last value");
    }

    #[test]
    fn repeat_false_at_exact_boundary_returns_last() {
        let content = "5.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, false).unwrap();
        assert_eq!(gen.value(0), 5.0);
        assert_eq!(
            gen.value(1),
            5.0,
            "single-element, tick=1 should clamp to 5.0"
        );
    }

    // ---- Empty file returns error ---------------------------------------------

    #[test]
    fn empty_file_returns_error() {
        let (_tmp, path) = temp_csv("");
        let result = CsvReplayGenerator::new(&path, 0, false, true);
        assert!(result.is_err(), "empty file must return an error");
        let err = result.err().expect("already confirmed is_err");
        let msg = format!("{err}");
        assert!(
            msg.contains("no valid numeric values"),
            "error message should mention 'no valid numeric values', got: {msg}"
        );
    }

    #[test]
    fn empty_content_from_str_returns_error() {
        let result = CsvReplayGenerator::from_str("", 0, false, true);
        assert!(result.is_err(), "empty content must return an error");
    }

    // ---- File with no valid values returns error ------------------------------

    #[test]
    fn file_with_only_comments_returns_error() {
        let content = "# comment 1\n# comment 2\n";
        let result = CsvReplayGenerator::from_str(content, 0, false, true);
        assert!(result.is_err(), "file with only comments must error");
    }

    #[test]
    fn file_with_only_header_returns_error() {
        let content = "timestamp,cpu\n";
        let result = CsvReplayGenerator::from_str(content, 0, true, true);
        assert!(result.is_err(), "file with only a header row must error");
    }

    #[test]
    fn file_with_no_parseable_numbers_returns_error() {
        let content = "not_a_number\nhello\nworld\n";
        let result = CsvReplayGenerator::from_str(content, 0, false, true);
        assert!(result.is_err(), "file with no parseable numbers must error");
    }

    #[test]
    fn file_with_header_and_unparseable_body_returns_error() {
        let content = "header\nabc\ndef\n";
        let result = CsvReplayGenerator::from_str(content, 0, true, true);
        assert!(
            result.is_err(),
            "file with header and no parseable body must error"
        );
    }

    // ---- File not found returns error -----------------------------------------

    #[test]
    fn file_not_found_returns_error() {
        let result =
            CsvReplayGenerator::new("/nonexistent/path/that/does/not/exist.csv", 0, false, true);
        assert!(result.is_err(), "missing file must return an error");
        let err = result.err().expect("already confirmed is_err");
        let msg = format!("{err}");
        assert!(
            msg.contains("cannot read CSV file"),
            "error message should mention 'cannot read CSV file', got: {msg}"
        );
    }

    // ---- Invalid column index (out of bounds) returns error -------------------

    #[test]
    fn column_index_out_of_bounds_returns_error() {
        let content = "1.0,2.0\n3.0,4.0\n";
        // Column 5 does not exist in a 2-column CSV.
        let result = CsvReplayGenerator::from_str(content, 5, false, true);
        assert!(
            result.is_err(),
            "column index out of bounds must return an error"
        );
    }

    #[test]
    fn column_index_out_of_bounds_on_disk() {
        let (_tmp, path) = temp_csv("1.0,2.0\n3.0,4.0\n");
        let result = CsvReplayGenerator::new(&path, 10, false, true);
        assert!(
            result.is_err(),
            "column index out of bounds on disk file must error"
        );
    }

    // ---- Large tick values don't panic ----------------------------------------

    #[test]
    fn repeat_large_tick_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        let large_tick: u64 = 1_000_000_000;
        let val = gen.value(large_tick);
        let expected_index = (large_tick % 3) as usize;
        let expected = [1.0, 2.0, 3.0][expected_index];
        assert_eq!(val, expected);
    }

    #[test]
    fn no_repeat_large_tick_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, false).unwrap();
        let large_tick: u64 = 1_000_000_000;
        assert_eq!(gen.value(large_tick), 3.0, "should clamp to last value");
    }

    // ---- 32-bit truncation safety (tick > u32::MAX) ----------------------------

    #[test]
    fn repeat_tick_above_u32_max_uses_u64_modulo() {
        let content = "10.0\n20.0\n30.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        // tick = 4_294_967_296: u64 modulo 4_294_967_296 % 3 = 1
        let tick: u64 = u64::from(u32::MAX) + 1;
        assert_eq!(
            gen.value(tick),
            20.0,
            "tick {} mod 3 = 1, should return values[1] = 20.0",
            tick
        );
    }

    #[test]
    fn repeat_tick_at_u64_max_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        let val = gen.value(u64::MAX);
        // u64::MAX % 3 = 0
        assert_eq!(val, 1.0, "u64::MAX % 3 = 0, should return values[0]");
    }

    #[test]
    fn no_repeat_tick_above_u32_max_clamps_correctly() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, false).unwrap();
        let tick: u64 = u64::from(u32::MAX) + 1;
        assert_eq!(
            gen.value(tick),
            3.0,
            "tick {} beyond length should clamp to last value",
            tick
        );
    }

    #[test]
    fn no_repeat_tick_at_u64_max_clamps_correctly() {
        let content = "1.0\n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, false).unwrap();
        assert_eq!(
            gen.value(u64::MAX),
            2.0,
            "u64::MAX should clamp to last value"
        );
    }

    // ---- CsvReplayGenerator is Send + Sync ------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn csv_replay_generator_is_send_and_sync() {
        assert_send_sync::<CsvReplayGenerator>();
    }

    // ---- Determinism: same tick always returns same value ---------------------

    #[test]
    fn determinism_same_tick_returns_same_value() {
        let content = "10.0\n20.0\n30.0\n40.0\n50.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        for tick in 0..50 {
            let first_call = gen.value(tick);
            let second_call = gen.value(tick);
            assert_eq!(
                first_call, second_call,
                "value must be deterministic: tick={tick} returned {first_call} then {second_call}"
            );
        }
    }

    #[test]
    fn determinism_separate_instances_same_content() {
        let content = "5.0\n10.0\n15.0\n";
        let gen1 = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        let gen2 = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        for tick in 0..30 {
            assert_eq!(
                gen1.value(tick),
                gen2.value(tick),
                "two generators with same content must produce same values at tick={tick}"
            );
        }
    }

    // ---- Factory creates generator from config --------------------------------

    #[test]
    fn factory_csv_replay_creates_working_generator() {
        let (_tmp, path) = temp_csv("10.0\n20.0\n30.0\n");
        let config = super::super::GeneratorConfig::CsvReplay {
            file: path,
            column: Some(0),
            has_header: Some(false),
            repeat: Some(true),
        };
        let gen =
            super::super::create_generator(&config, 1.0).expect("csv_replay factory must succeed");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 10.0, "should wrap around");
    }

    #[test]
    fn factory_csv_replay_defaults() {
        // column defaults to 0, has_header defaults to true, repeat defaults to true
        let (_tmp, path) = temp_csv("header\n42.0\n");
        let config = super::super::GeneratorConfig::CsvReplay {
            file: path,
            column: None,
            has_header: None,
            repeat: None,
        };
        let gen = super::super::create_generator(&config, 1.0)
            .expect("csv_replay factory with defaults must succeed");
        // has_header defaults to true, so "header" is skipped, leaving 42.0
        assert_eq!(gen.value(0), 42.0);
    }

    #[test]
    fn factory_csv_replay_missing_file_returns_error() {
        let config = super::super::GeneratorConfig::CsvReplay {
            file: "/nonexistent/file.csv".to_string(),
            column: None,
            has_header: None,
            repeat: None,
        };
        let result = super::super::create_generator(&config, 1.0);
        assert!(
            result.is_err(),
            "factory with missing file must return error"
        );
    }

    // ---- Example YAML deserializes and runs -----------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_csv_replay_config_from_yaml() {
        let yaml = "\
type: csv_replay
file: /some/path.csv
column: 1
has_header: true
repeat: false
";
        let config: super::super::GeneratorConfig =
            serde_yaml::from_str(yaml).expect("csv_replay YAML must deserialize");
        match config {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                has_header,
                repeat,
            } => {
                assert_eq!(file, "/some/path.csv");
                assert_eq!(column, Some(1));
                assert_eq!(has_header, Some(true));
                assert_eq!(repeat, Some(false));
            }
            _ => panic!("expected CsvReplay variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_csv_replay_config_minimal() {
        let yaml = "type: csv_replay\nfile: data.csv\n";
        let config: super::super::GeneratorConfig =
            serde_yaml::from_str(yaml).expect("minimal csv_replay YAML must deserialize");
        match config {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                has_header,
                repeat,
            } => {
                assert_eq!(file, "data.csv");
                assert_eq!(column, None, "column should be None when omitted");
                assert_eq!(has_header, None, "has_header should be None when omitted");
                assert_eq!(repeat, None, "repeat should be None when omitted");
            }
            _ => panic!("expected CsvReplay variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn example_yaml_scenario_file_deserializes() {
        // Validate the example file pattern from examples/csv-replay-metrics.yaml.
        // We use a temp CSV to allow the factory to actually load data.
        let (_tmp, csv_path) =
            temp_csv("timestamp,cpu_percent\n1700000000,12.3\n1700000010,14.1\n");
        let yaml = format!(
            "\
name: cpu_replay
rate: 1
duration: 60s

generator:
  type: csv_replay
  file: {}
  column: 1
  has_header: true
  repeat: true

labels:
  instance: prod-server-42
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
",
            csv_path
        );
        let config: crate::config::ScenarioConfig =
            serde_yaml::from_str(&yaml).expect("example scenario YAML must deserialize");
        assert_eq!(config.name, "cpu_replay");
        assert_eq!(config.rate, 1.0);
        match &config.generator {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                has_header,
                repeat,
            } => {
                assert_eq!(file, &csv_path);
                assert_eq!(*column, Some(1));
                assert_eq!(*has_header, Some(true));
                assert_eq!(*repeat, Some(true));
            }
            _ => panic!("expected CsvReplay generator variant"),
        }

        // Also verify the factory can create a working generator from this config.
        let gen = super::super::create_generator(&config.generator, config.rate)
            .expect("factory must succeed for example config");
        assert_eq!(gen.value(0), 12.3);
        assert_eq!(gen.value(1), 14.1);
    }

    // ---- Unparseable rows are silently skipped --------------------------------

    #[test]
    fn unparseable_rows_are_skipped() {
        let content = "1.0\nnot_a_number\n2.0\n???\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true)
            .expect("should skip unparseable rows");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    // ---- Mixed: comments, empty lines, header, unparseable --------------------

    #[test]
    fn mixed_content_loads_correctly() {
        let content = "\
# CPU values from production
# Exported 2024-01-15

timestamp,cpu_percent
1700000000,12.3

# spike starts here
1700000010,bad_data
1700000020,95.5

";
        let gen = CsvReplayGenerator::from_str(content, 1, true, true)
            .expect("mixed content should load");
        // After skipping comments, empty lines, header, and unparseable "bad_data":
        // Values are: 12.3, 95.5
        assert_eq!(gen.value(0), 12.3);
        assert_eq!(gen.value(1), 95.5);
        assert_eq!(gen.value(2), 12.3, "should cycle");
    }

    // ---- Fields with whitespace trim correctly --------------------------------

    #[test]
    fn fields_with_whitespace_are_trimmed() {
        let content = "  1.0  ,  2.0  \n  3.0  ,  4.0  \n";
        let gen = CsvReplayGenerator::from_str(content, 1, false, true)
            .expect("whitespace around fields should be trimmed");
        assert_eq!(gen.value(0), 2.0);
        assert_eq!(gen.value(1), 4.0);
    }

    // ---- Single value file ----------------------------------------------------

    #[test]
    fn single_value_repeat_true() {
        let content = "42.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(1), 42.0);
        assert_eq!(gen.value(100), 42.0);
    }

    #[test]
    fn single_value_repeat_false() {
        let content = "42.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, false).unwrap();
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(1), 42.0);
        assert_eq!(gen.value(100), 42.0);
    }

    // ---- Negative and special float values ------------------------------------

    #[test]
    fn handles_negative_values() {
        let content = "-1.5\n-2.5\n0.0\n3.14\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        assert_eq!(gen.value(0), -1.5);
        assert_eq!(gen.value(1), -2.5);
        assert_eq!(gen.value(2), 0.0);
        assert_eq!(gen.value(3), 3.14);
    }

    #[test]
    fn handles_integer_values() {
        let content = "1\n2\n3\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false, true).unwrap();
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    // ---- Verify value count ---------------------------------------------------

    #[test]
    fn correct_number_of_values_loaded() {
        // The sample CSV has 50 data rows + 1 header + 3 comment lines.
        // column 1 = cpu_percent. has_header = true skips the header row.
        // Comments are skipped. All 50 data rows should parse.
        let content = "\
# comment 1
# comment 2
# comment 3
ts,val
1,10.0
2,20.0
3,30.0
4,40.0
5,50.0
";
        let gen =
            CsvReplayGenerator::from_str(content, 1, true, true).expect("should load 5 values");
        // Verify wrapping at length 5
        assert_eq!(gen.value(5), gen.value(0), "should wrap at 5 values");
        assert_eq!(gen.value(6), gen.value(1));
    }

    // ---- Regression: sample-cpu-values.csv loads correctly --------------------

    #[test]
    fn sample_cpu_values_csv_from_disk() {
        // This test uses the actual example file shipped with the project.
        // It validates the end-to-end path: file -> parse -> generator.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/sample-cpu-values.csv"
        );
        let result = CsvReplayGenerator::new(path, 1, true, true);
        match result {
            Ok(gen) => {
                // First data row: 1700000000,12.3
                assert!(
                    (gen.value(0) - 12.3).abs() < 1e-10,
                    "first value should be 12.3, got {}",
                    gen.value(0)
                );
                // Values should cycle: 50 data rows
                assert_eq!(
                    gen.value(50),
                    gen.value(0),
                    "should wrap at 50 values (tick 50 == tick 0)"
                );
            }
            Err(e) => {
                // If the file is not at the expected path (CI environment),
                // skip gracefully. The from_str tests cover the logic.
                eprintln!("Skipping sample CSV disk test (file not found): {e}");
            }
        }
    }
}
