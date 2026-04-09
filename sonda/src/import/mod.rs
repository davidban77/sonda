//! CSV import: analyze time-series data and generate parameterized scenarios.
//!
//! This module implements the `sonda import` subcommand. It reads a CSV file,
//! detects dominant time-series patterns (steady, spike, climb, flap, etc.),
//! and either prints a human-readable analysis or generates a portable scenario
//! YAML that uses sonda generators instead of `csv_replay`.
//!
//! All pattern detection is statistical analysis that lives in the CLI crate.
//! It does NOT belong in sonda-core.

pub mod csv_reader;
pub mod pattern;
pub mod yaml_gen;

use std::path::Path;

use anyhow::{bail, Context, Result};

use csv_reader::{read_csv, CsvData};
use pattern::detect_pattern;
use yaml_gen::{pattern_to_spec, render_yaml};

/// Run the import command in analysis mode (`--analyze`).
///
/// Reads the CSV file, detects patterns for each column, and prints a
/// human-readable summary to stdout.
pub fn run_analyze(path: &Path, selected_columns: Option<&[usize]>) -> Result<()> {
    let data = read_csv(path, selected_columns)?;
    print_analysis(&data);
    Ok(())
}

/// Run the import command in YAML generation mode (`-o <output.yaml>`).
///
/// Reads the CSV file, detects patterns, generates scenario YAML, and writes
/// it to the specified output path.
pub fn run_generate(
    path: &Path,
    output: &Path,
    selected_columns: Option<&[usize]>,
    rate: f64,
    duration: &str,
) -> Result<()> {
    let data = read_csv(path, selected_columns)?;
    let yaml = generate_yaml(&data, rate, duration);
    std::fs::write(output, &yaml)
        .with_context(|| format!("failed to write output YAML to {}", output.display()))?;
    eprintln!("wrote scenario to {}", output.display());
    Ok(())
}

/// Run the import command in run mode (`--run`).
///
/// Reads the CSV, detects patterns, generates scenario YAML, writes it to
/// a temporary file, and immediately executes it via the standard scenario
/// loading path.
///
/// Returns the generated YAML string and the path to the temporary file
/// for the caller to pass to the scenario runner.
pub fn run_generate_and_execute(
    path: &Path,
    selected_columns: Option<&[usize]>,
    rate: f64,
    duration: &str,
) -> Result<String> {
    let data = read_csv(path, selected_columns)?;
    let yaml = generate_yaml(&data, rate, duration);
    Ok(yaml)
}

/// Print a human-readable analysis of detected patterns.
fn print_analysis(data: &CsvData) {
    println!("CSV Import Analysis");
    println!("{}", "=".repeat(60));
    println!();

    for (i, (col, vals)) in data.columns.iter().zip(data.values.iter()).enumerate() {
        let fallback_name = format!("column_{}", col.index);
        let name = col.metric_name.as_deref().unwrap_or(&fallback_name);
        println!("Column {} (index {}): {}", i + 1, col.index, name);

        if !col.labels.is_empty() {
            let mut sorted_labels: Vec<_> = col.labels.iter().collect();
            sorted_labels.sort_by_key(|(k, _)| *k);
            let label_str: Vec<String> = sorted_labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{v}\""))
                .collect();
            println!("  Labels: {{{}}}", label_str.join(", "));
        }

        if vals.is_empty() {
            println!("  No numeric data");
            println!();
            continue;
        }

        // Basic stats.
        let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        println!("  Data points: {}", vals.len());
        println!("  Range: [{min:.2}, {max:.2}]  Mean: {mean:.2}");

        // Detected pattern.
        let pattern = detect_pattern(vals);
        println!("  Detected pattern: {pattern}");
        println!();
    }
}

/// Generate scenario YAML from CSV data.
fn generate_yaml(data: &CsvData, rate: f64, duration: &str) -> String {
    let specs: Vec<_> = data
        .columns
        .iter()
        .zip(data.values.iter())
        .map(|(col, vals)| {
            let pattern = detect_pattern(vals);
            pattern_to_spec(&pattern, col, rate, duration)
        })
        .collect();

    render_yaml(&specs, rate, duration)
}

/// Parse a comma-separated list of column indices.
///
/// Returns `None` if the input is `None`. Returns an error if any value
/// cannot be parsed as a `usize`.
pub fn parse_column_list(input: Option<&str>) -> Result<Option<Vec<usize>>> {
    match input {
        None => Ok(None),
        Some(s) => {
            let indices: Vec<usize> = s
                .split(',')
                .map(|part| {
                    part.trim()
                        .parse::<usize>()
                        .with_context(|| format!("invalid column index: {part:?}"))
                })
                .collect::<Result<Vec<_>>>()?;
            if indices.is_empty() {
                bail!("--columns requires at least one column index");
            }
            Ok(Some(indices))
        }
    }
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
    // parse_column_list
    // -----------------------------------------------------------------------

    #[test]
    fn parse_column_list_none_returns_none() {
        assert!(parse_column_list(None).unwrap().is_none());
    }

    #[test]
    fn parse_column_list_single() {
        let result = parse_column_list(Some("3")).unwrap().unwrap();
        assert_eq!(result, vec![3]);
    }

    #[test]
    fn parse_column_list_multiple() {
        let result = parse_column_list(Some("1,3,5")).unwrap().unwrap();
        assert_eq!(result, vec![1, 3, 5]);
    }

    #[test]
    fn parse_column_list_with_spaces() {
        let result = parse_column_list(Some("1 , 3 , 5")).unwrap().unwrap();
        assert_eq!(result, vec![1, 3, 5]);
    }

    #[test]
    fn parse_column_list_invalid_returns_error() {
        let result = parse_column_list(Some("1,abc,3"));
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Integration: analyze mode
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_steady_csv_succeeds() {
        let csv = "timestamp,cpu\n1000,50.1\n2000,49.9\n3000,50.2\n4000,49.8\n5000,50.0\n";
        let f = write_temp_csv(csv);
        let result = run_analyze(f.path(), None);
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // Integration: generate mode
    // -----------------------------------------------------------------------

    #[test]
    fn generate_yaml_from_csv_produces_valid_output() {
        let csv = "timestamp,cpu,mem\n1000,50.1,80.0\n2000,49.9,79.5\n3000,50.2,80.1\n4000,49.8,79.9\n5000,50.0,80.2\n";
        let f = write_temp_csv(csv);
        let out = tempfile::NamedTempFile::new().expect("create output file");
        let result = run_generate(f.path(), out.path(), None, 1.0, "60s");
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).expect("read output");
        assert!(
            content.contains("scenarios:"),
            "multi-column should use scenarios wrapper"
        );
        assert!(content.contains("name: cpu"));
        assert!(content.contains("name: mem"));
    }

    #[test]
    fn generate_single_column_produces_flat_yaml() {
        let csv = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n";
        let f = write_temp_csv(csv);
        let out = tempfile::NamedTempFile::new().expect("create output file");
        let result = run_generate(f.path(), out.path(), None, 1.0, "60s");
        assert!(result.is_ok());

        let content = std::fs::read_to_string(out.path()).expect("read output");
        assert!(
            !content.contains("scenarios:"),
            "single-column should be flat YAML"
        );
        assert!(content.contains("name: cpu"));
    }

    // -----------------------------------------------------------------------
    // Integration: generate-and-execute mode
    // -----------------------------------------------------------------------

    #[test]
    fn generate_and_execute_returns_yaml_string() {
        let csv = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n";
        let f = write_temp_csv(csv);
        let result = run_generate_and_execute(f.path(), None, 1.0, "60s");
        assert!(result.is_ok());

        let yaml = result.unwrap();
        assert!(yaml.contains("name: cpu"));
        assert!(yaml.contains("type: steady"));
    }

    // -----------------------------------------------------------------------
    // Integration: Grafana CSV with labels
    // -----------------------------------------------------------------------

    #[test]
    fn grafana_csv_labels_preserved_in_yaml() {
        let csv = concat!(
            r#""Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}""#,
            "\n",
            "1000,1\n",
            "2000,1\n",
            "3000,1\n",
            "4000,1\n",
            "5000,1\n",
        );
        let f = write_temp_csv(csv);
        let yaml = run_generate_and_execute(f.path(), None, 1.0, "60s").unwrap();
        assert!(yaml.contains("instance:"));
        assert!(yaml.contains("job:"));
    }

    // -----------------------------------------------------------------------
    // Error: nonexistent file
    // -----------------------------------------------------------------------

    #[test]
    fn nonexistent_file_returns_clear_error() {
        let result = run_analyze(Path::new("/nonexistent/file.csv"), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("failed to read"), "got: {msg}");
    }

    // -----------------------------------------------------------------------
    // Integration: column selection
    // -----------------------------------------------------------------------

    #[test]
    fn column_selection_generates_yaml_for_selected_only() {
        let csv = "timestamp,cpu,mem,disk\n1000,50.0,80.0,55.0\n2000,50.1,79.5,56.0\n3000,49.9,80.1,57.0\n";
        let f = write_temp_csv(csv);
        let yaml = run_generate_and_execute(f.path(), Some(&[1, 3]), 1.0, "60s").unwrap();
        assert!(yaml.contains("name: cpu"));
        assert!(yaml.contains("name: disk"));
        assert!(!yaml.contains("name: mem"));
    }
}
