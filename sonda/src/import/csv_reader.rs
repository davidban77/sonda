//! CSV file reading and column data extraction for the `import` subcommand.
//!
//! Reads a CSV file, detects headers (Grafana-style or plain), and extracts
//! numeric data for selected columns. Reuses sonda-core's CSV header parsing
//! infrastructure for header detection and label extraction.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use sonda_core::generator::csv_header::{
    is_header_line, parse_header_row, split_csv_header_fields,
};

/// Parsed metadata for a single CSV column.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    /// Zero-based column index in the CSV file.
    pub index: usize,
    /// Metric name extracted from the header (if present).
    pub metric_name: Option<String>,
    /// Labels extracted from Grafana-style `{key="value"}` headers.
    pub labels: HashMap<String, String>,
}

/// All data extracted from a CSV file for import analysis.
#[derive(Debug)]
pub struct CsvData {
    /// Metadata for each selected column.
    pub columns: Vec<ColumnMeta>,
    /// Numeric values for each selected column, parallel to `columns`.
    pub values: Vec<Vec<f64>>,
}

/// Read a CSV file and extract numeric data for the specified columns.
///
/// If `selected_columns` is `None`, all non-timestamp columns (index > 0)
/// are processed. If `Some`, only the specified column indices are used.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read.
/// - No header is detected.
/// - No numeric data is found in any selected column.
/// - A selected column index is out of range.
pub fn read_csv(path: &Path, selected_columns: Option<&[usize]>) -> Result<CsvData> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read CSV file: {}", path.display()))?;

    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    if lines.is_empty() {
        bail!("CSV file is empty: {}", path.display());
    }

    // Detect and parse header.
    let (header_line, data_start) = if is_header_line(lines[0]) {
        (Some(lines[0]), 1)
    } else {
        (None, 0)
    };

    // Parse headers to get column metadata.
    let parsed_headers = if let Some(header) = header_line {
        parse_header_row(header).map_err(|e| anyhow::anyhow!("failed to parse CSV header: {e}"))?
    } else {
        Vec::new()
    };

    // Determine column count from the first data row.
    let first_data = lines.get(data_start);
    let total_columns = if let Some(line) = first_data {
        split_csv_header_fields(line).len()
    } else if !parsed_headers.is_empty() {
        parsed_headers.len()
    } else {
        bail!("CSV file has no data rows: {}", path.display());
    };

    // Determine which columns to process.
    let target_indices: Vec<usize> = match selected_columns {
        Some(cols) => {
            for &idx in cols {
                if idx == 0 {
                    bail!("column 0 is the timestamp column and cannot be imported");
                }
                if idx >= total_columns {
                    bail!(
                        "column index {idx} is out of range (file has {total_columns} columns, indices 0..{})",
                        total_columns - 1
                    );
                }
            }
            cols.to_vec()
        }
        None => {
            // All non-timestamp columns.
            if total_columns <= 1 {
                // Single-column CSV: use column 0 as data.
                vec![0]
            } else {
                (1..total_columns).collect()
            }
        }
    };

    if target_indices.is_empty() {
        bail!("no columns selected for import");
    }

    // Build column metadata.
    let columns: Vec<ColumnMeta> = target_indices
        .iter()
        .map(|&idx| {
            let (name, labels) = if idx < parsed_headers.len() {
                let h = &parsed_headers[idx];
                (h.metric_name.clone(), h.labels.clone())
            } else {
                (None, HashMap::new())
            };
            ColumnMeta {
                index: idx,
                metric_name: name,
                labels,
            }
        })
        .collect();

    // Extract numeric values per column.
    let mut values: Vec<Vec<f64>> = vec![Vec::new(); target_indices.len()];

    for line in &lines[data_start..] {
        let fields: Vec<&str> = line.split(',').collect();
        for (col_pos, &col_idx) in target_indices.iter().enumerate() {
            if let Some(field) = fields.get(col_idx) {
                if let Ok(v) = field.trim().parse::<f64>() {
                    if v.is_finite() {
                        values[col_pos].push(v);
                    }
                }
            }
        }
    }

    // Check that at least one column has data.
    let has_data = values.iter().any(|v| !v.is_empty());
    if !has_data {
        bail!(
            "no numeric data found in selected columns of {}",
            path.display()
        );
    }

    Ok(CsvData { columns, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp_csv(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        f.flush().expect("flush temp file");
        f
    }

    // -----------------------------------------------------------------------
    // Happy path: plain CSV
    // -----------------------------------------------------------------------

    #[test]
    fn read_plain_csv_all_columns() {
        let csv = "timestamp,cpu,mem\n1000,42.5,80.1\n2000,43.0,79.5\n3000,41.8,81.2\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).expect("must succeed");

        assert_eq!(data.columns.len(), 2);
        assert_eq!(data.columns[0].metric_name.as_deref(), Some("cpu"));
        assert_eq!(data.columns[1].metric_name.as_deref(), Some("mem"));
        assert_eq!(data.values[0], vec![42.5, 43.0, 41.8]);
        assert_eq!(data.values[1], vec![80.1, 79.5, 81.2]);
    }

    #[test]
    fn read_plain_csv_selected_columns() {
        let csv = "timestamp,cpu,mem,disk\n1000,42.5,80.1,55.0\n2000,43.0,79.5,56.0\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), Some(&[1, 3])).expect("must succeed");

        assert_eq!(data.columns.len(), 2);
        assert_eq!(data.columns[0].metric_name.as_deref(), Some("cpu"));
        assert_eq!(data.columns[1].metric_name.as_deref(), Some("disk"));
        assert_eq!(data.values[0], vec![42.5, 43.0]);
        assert_eq!(data.values[1], vec![55.0, 56.0]);
    }

    // -----------------------------------------------------------------------
    // Happy path: Grafana-style CSV
    // -----------------------------------------------------------------------

    #[test]
    fn read_grafana_csv_with_labels() {
        let csv = concat!(
            r#""Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}""#,
            "\n",
            "1000,1\n",
            "2000,1\n",
            "3000,0\n",
        );
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).expect("must succeed");

        assert_eq!(data.columns.len(), 1);
        assert_eq!(data.columns[0].metric_name.as_deref(), Some("up"));
        assert_eq!(
            data.columns[0].labels.get("instance").map(|s| s.as_str()),
            Some("localhost:9090")
        );
        assert_eq!(
            data.columns[0].labels.get("job").map(|s| s.as_str()),
            Some("prometheus")
        );
        assert_eq!(data.values[0], vec![1.0, 1.0, 0.0]);
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn read_nonexistent_file_returns_error() {
        let result = read_csv(Path::new("/nonexistent/file.csv"), None);
        assert!(result.is_err());
    }

    #[test]
    fn read_empty_csv_returns_error() {
        let f = write_temp_csv("");
        let result = read_csv(f.path(), None);
        assert!(result.is_err());
    }

    #[test]
    fn column_zero_is_rejected() {
        let csv = "timestamp,cpu\n1000,42.5\n";
        let f = write_temp_csv(csv);
        let result = read_csv(f.path(), Some(&[0]));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("timestamp"), "got: {msg}");
    }

    #[test]
    fn out_of_range_column_is_rejected() {
        let csv = "timestamp,cpu\n1000,42.5\n";
        let f = write_temp_csv(csv);
        let result = read_csv(f.path(), Some(&[5]));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("out of range"), "got: {msg}");
    }

    // -----------------------------------------------------------------------
    // Edge case: single-column CSV (no timestamp)
    // -----------------------------------------------------------------------

    #[test]
    fn single_column_numeric_csv() {
        let csv = "42.5\n43.0\n41.8\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).expect("must succeed");

        assert_eq!(data.columns.len(), 1);
        assert_eq!(data.columns[0].index, 0);
        assert_eq!(data.values[0], vec![42.5, 43.0, 41.8]);
    }

    // -----------------------------------------------------------------------
    // Edge case: CSV with comment lines
    // -----------------------------------------------------------------------

    #[test]
    fn csv_with_comments_and_empty_lines() {
        let csv = "# This is a comment\ntimestamp,cpu\n\n1000,42.5\n# another comment\n2000,43.0\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).expect("must succeed");

        assert_eq!(data.values[0], vec![42.5, 43.0]);
    }

    // -----------------------------------------------------------------------
    // Edge case: no numeric data in selected column
    // -----------------------------------------------------------------------

    #[test]
    fn no_numeric_data_returns_error() {
        let csv = "timestamp,status\n1000,ok\n2000,fail\n";
        let f = write_temp_csv(csv);
        let result = read_csv(f.path(), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("no numeric data"), "got: {msg}");
    }
}
