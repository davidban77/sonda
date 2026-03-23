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
        let index = if self.repeat {
            (tick as usize) % len
        } else {
            (tick as usize).min(len - 1)
        };
        self.values[index]
    }
}
