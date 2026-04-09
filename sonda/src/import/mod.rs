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
use owo_colors::OwoColorize;
use owo_colors::Stream::{Stderr, Stdout};

use csv_reader::{read_csv, CsvData};
use pattern::{detect_pattern, Pattern};
use yaml_gen::{pattern_to_spec, render_yaml};

/// Width used for horizontal rules in the import flow.
///
/// Matches the `RULE_WIDTH` / `SECTION_WIDTH` used by `sonda init` for
/// visual consistency across subcommands.
const RULE_WIDTH: usize = 45;

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
/// it to the specified output path. Prints a styled success summary to stderr.
pub fn run_generate(
    path: &Path,
    output: &Path,
    selected_columns: Option<&[usize]>,
    rate: f64,
    duration: &str,
) -> Result<()> {
    let data = read_csv(path, selected_columns)?;
    let patterns = detect_all_patterns(&data);
    let yaml = generate_yaml_from_patterns(&data, &patterns, rate, duration);
    std::fs::write(output, &yaml)
        .with_context(|| format!("failed to write output YAML to {}", output.display()))?;
    print_generate_success(output, &data, &patterns);
    Ok(())
}

/// Run the import command in run mode (`--run`).
///
/// Reads the CSV, detects patterns, generates scenario YAML, and returns it
/// for the caller to pass to the scenario runner. Prints a styled detection
/// summary to stderr so the user sees what patterns were identified.
///
/// Returns the generated YAML string.
pub fn run_generate_and_execute(
    path: &Path,
    selected_columns: Option<&[usize]>,
    rate: f64,
    duration: &str,
) -> Result<String> {
    let data = read_csv(path, selected_columns)?;
    let patterns = detect_all_patterns(&data);
    print_run_detection_summary(&data, &patterns);
    let yaml = generate_yaml_from_patterns(&data, &patterns, rate, duration);
    Ok(yaml)
}

/// Detect the dominant pattern for every column in the CSV data.
///
/// Returns a `Vec<Option<Pattern>>` parallel to `data.columns`. Columns
/// with no numeric data produce `None`.
fn detect_all_patterns(data: &CsvData) -> Vec<Option<Pattern>> {
    data.values
        .iter()
        .map(|vals| {
            if vals.is_empty() {
                None
            } else {
                Some(detect_pattern(vals))
            }
        })
        .collect()
}

/// Print a styled human-readable analysis of detected patterns to stdout.
///
/// Uses `Stream::Stdout` for coloring because analysis output is the primary
/// payload of `--analyze` mode (piping to a file is valid).
fn print_analysis(data: &CsvData) {
    let title_style = owo_colors::Style::new().bold().cyan();
    let rule: String = "\u{2500}".repeat(RULE_WIDTH);

    println!("\n{}", rule.if_supports_color(Stdout, |t| t.dimmed()));
    println!(
        "  {}",
        "CSV Import Analysis".if_supports_color(Stdout, |t| t.style(title_style)),
    );
    println!("{}\n", rule.if_supports_color(Stdout, |t| t.dimmed()));

    let thin_rule: String = "\u{2500}".repeat(RULE_WIDTH.saturating_sub(4));

    for (i, (col, vals)) in data.columns.iter().zip(data.values.iter()).enumerate() {
        if i > 0 {
            println!("  {}", thin_rule.if_supports_color(Stdout, |t| t.dimmed()));
            println!();
        }

        let fallback_name = format!("column_{}", col.index);
        let name = col.metric_name.as_deref().unwrap_or(&fallback_name);
        let index_label = format!("Column {} (index {}):", i + 1, col.index);
        println!(
            "  {} {}",
            index_label.if_supports_color(Stdout, |t| t.dimmed()),
            name.if_supports_color(Stdout, |t| t.bold()),
        );

        if !col.labels.is_empty() {
            let mut sorted_labels: Vec<_> = col.labels.iter().collect();
            sorted_labels.sort_by_key(|(k, _)| *k);
            let label_str: Vec<String> = sorted_labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{v}\""))
                .collect();
            println!(
                "  {} {{{}}}",
                "labels:".if_supports_color(Stdout, |t| t.dimmed()),
                label_str.join(", "),
            );
        }

        if vals.is_empty() {
            println!(
                "  {}",
                "No numeric data".if_supports_color(Stdout, |t| t.dimmed()),
            );
            println!();
            continue;
        }

        // Basic stats.
        let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        println!(
            "  {} {}   {} [{min:.2}, {max:.2}]   {} {mean:.2}",
            "points:".if_supports_color(Stdout, |t| t.dimmed()),
            vals.len(),
            "range:".if_supports_color(Stdout, |t| t.dimmed()),
            "mean:".if_supports_color(Stdout, |t| t.dimmed()),
        );

        // Detected pattern.
        let pattern = detect_pattern(vals);
        let pattern_style = owo_colors::Style::new().bold().cyan();
        println!(
            "  {} {}",
            "detected:".if_supports_color(Stdout, |t| t.dimmed()),
            pattern.if_supports_color(Stdout, |t| t.style(pattern_style)),
        );
        println!();
    }
}

/// Generate scenario YAML from pre-computed patterns.
fn generate_yaml_from_patterns(
    data: &CsvData,
    patterns: &[Option<Pattern>],
    rate: f64,
    duration: &str,
) -> String {
    let specs: Vec<_> = data
        .columns
        .iter()
        .zip(patterns.iter())
        .filter_map(|(col, pat)| {
            let pattern = pat.as_ref()?;
            Some(pattern_to_spec(pattern, col, rate, duration))
        })
        .collect();

    render_yaml(&specs, rate, duration)
}

/// Print a styled success summary after writing a scenario YAML file.
///
/// Matches the visual style used by `sonda init`'s success block: green
/// checkmark, bold file path, and a suggestion for how to run the scenario.
fn print_generate_success(output: &Path, data: &CsvData, patterns: &[Option<Pattern>]) {
    let bold = owo_colors::Style::new().bold();
    let green_bold = owo_colors::Style::new().green().bold();
    let dimmed = owo_colors::Style::new().dimmed();
    let rule: String = "\u{2500}".repeat(RULE_WIDTH);

    let col_count = data.columns.len();
    let pattern_summary = build_pattern_summary(data, patterns);

    eprintln!("\n{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!(
        "  {} {}",
        "\u{2714}".if_supports_color(Stderr, |t| t.style(green_bold)),
        "Scenario written".if_supports_color(Stderr, |t| t.style(bold)),
    );
    eprintln!();

    let file_label = "file:".if_supports_color(Stderr, |t| t.style(dimmed));
    let file_value = output.display().to_string();
    let file_styled = file_value.if_supports_color(Stderr, |t| t.style(bold));
    eprintln!("  {file_label}     {file_styled}");

    let cols_label = "columns:".if_supports_color(Stderr, |t| t.style(dimmed));
    eprintln!("  {cols_label}  {col_count}");

    let det_label = "detected:".if_supports_color(Stderr, |t| t.style(dimmed));
    eprintln!("  {det_label} {pattern_summary}");
    eprintln!();

    eprintln!(
        "  {}",
        "Run it with:".if_supports_color(Stderr, |t| t.style(dimmed)),
    );
    let path_display = output.display();
    if col_count == 1 {
        eprintln!("    sonda metrics --scenario {path_display}");
    }
    eprintln!("    sonda run --scenario {path_display}");
    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!();
}

/// Print a brief styled detection summary to stderr before run mode execution.
///
/// Gives the user confidence about what patterns were identified before
/// the scenario starts emitting events.
fn print_run_detection_summary(data: &CsvData, patterns: &[Option<Pattern>]) {
    let title_style = owo_colors::Style::new().bold().cyan();
    let rule: String = "\u{2500}".repeat(RULE_WIDTH);

    let col_count = data.columns.len();
    let pattern_summary = build_pattern_summary(data, patterns);

    eprintln!("\n{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
    eprintln!(
        "  {} {}",
        "sonda import --run".if_supports_color(Stderr, |t| t.style(title_style)),
        format!(
            "\u{2014} {col_count} column{}",
            if col_count == 1 { "" } else { "s" }
        )
        .if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!(
        "  {} {}",
        "detected:".if_supports_color(Stderr, |t| t.dimmed()),
        pattern_summary,
    );
    eprintln!("{}", rule.if_supports_color(Stderr, |t| t.dimmed()));
}

/// Build a compact `name (pattern), name (pattern)` summary string.
///
/// Used by both the generate success block and the run detection summary
/// to show what was detected.
fn build_pattern_summary(data: &CsvData, patterns: &[Option<Pattern>]) -> String {
    let bold_cyan = owo_colors::Style::new().bold().cyan();

    let parts: Vec<String> = data
        .columns
        .iter()
        .zip(patterns.iter())
        .map(|(col, pat)| {
            let fallback_name = format!("column_{}", col.index);
            let name = col.metric_name.as_deref().unwrap_or(&fallback_name);
            match pat {
                Some(p) => {
                    let pname = p.name();
                    let styled = pname.if_supports_color(Stderr, |t| t.style(bold_cyan));
                    format!("{name} ({styled})")
                }
                None => format!("{name} (no data)"),
            }
        })
        .collect();

    parts.join(", ")
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

    // -----------------------------------------------------------------------
    // detect_all_patterns
    // -----------------------------------------------------------------------

    #[test]
    fn detect_all_patterns_returns_parallel_vec() {
        let csv = "timestamp,cpu,mem\n1000,50.0,80.0\n2000,50.1,79.5\n3000,49.9,80.1\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).unwrap();
        let patterns = detect_all_patterns(&data);
        assert_eq!(patterns.len(), data.columns.len());
        assert!(patterns.iter().all(|p| p.is_some()));
    }

    #[test]
    fn detect_all_patterns_empty_column_returns_none() {
        let data = CsvData {
            columns: vec![csv_reader::ColumnMeta {
                index: 1,
                metric_name: Some("empty".to_string()),
                labels: std::collections::HashMap::new(),
            }],
            values: vec![vec![]],
        };
        let patterns = detect_all_patterns(&data);
        assert_eq!(patterns.len(), 1);
        assert!(patterns[0].is_none());
    }

    // -----------------------------------------------------------------------
    // build_pattern_summary
    // -----------------------------------------------------------------------

    #[test]
    fn build_pattern_summary_includes_column_names() {
        let data = CsvData {
            columns: vec![
                csv_reader::ColumnMeta {
                    index: 1,
                    metric_name: Some("cpu".to_string()),
                    labels: std::collections::HashMap::new(),
                },
                csv_reader::ColumnMeta {
                    index: 2,
                    metric_name: Some("mem".to_string()),
                    labels: std::collections::HashMap::new(),
                },
            ],
            values: vec![vec![50.0, 50.1, 49.9], vec![80.0, 79.5, 80.1]],
        };
        let patterns = detect_all_patterns(&data);
        let summary = build_pattern_summary(&data, &patterns);
        assert!(
            summary.contains("cpu"),
            "summary should contain 'cpu': {summary}"
        );
        assert!(
            summary.contains("mem"),
            "summary should contain 'mem': {summary}"
        );
    }

    #[test]
    fn build_pattern_summary_shows_no_data_for_none_pattern() {
        let data = CsvData {
            columns: vec![csv_reader::ColumnMeta {
                index: 1,
                metric_name: Some("empty".to_string()),
                labels: std::collections::HashMap::new(),
            }],
            values: vec![vec![]],
        };
        let patterns = vec![None];
        let summary = build_pattern_summary(&data, &patterns);
        assert!(
            summary.contains("no data"),
            "summary should show 'no data': {summary}"
        );
    }

    #[test]
    fn build_pattern_summary_uses_fallback_name() {
        let data = CsvData {
            columns: vec![csv_reader::ColumnMeta {
                index: 5,
                metric_name: None,
                labels: std::collections::HashMap::new(),
            }],
            values: vec![vec![1.0, 2.0, 3.0]],
        };
        let patterns = detect_all_patterns(&data);
        let summary = build_pattern_summary(&data, &patterns);
        assert!(
            summary.contains("column_5"),
            "summary should use fallback name: {summary}"
        );
    }

    // -----------------------------------------------------------------------
    // Styled output: smoke tests (no panics)
    // -----------------------------------------------------------------------

    #[test]
    fn print_analysis_does_not_panic_on_single_column() {
        let csv = "timestamp,cpu\n1000,50.0\n2000,50.1\n3000,49.9\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).unwrap();
        print_analysis(&data);
    }

    #[test]
    fn print_analysis_does_not_panic_on_multi_column() {
        let csv = "timestamp,cpu,mem\n1000,50.0,80.0\n2000,50.1,79.5\n3000,49.9,80.1\n";
        let f = write_temp_csv(csv);
        let data = read_csv(f.path(), None).unwrap();
        print_analysis(&data);
    }

    #[test]
    fn print_generate_success_does_not_panic() {
        let data = CsvData {
            columns: vec![csv_reader::ColumnMeta {
                index: 1,
                metric_name: Some("cpu".to_string()),
                labels: std::collections::HashMap::new(),
            }],
            values: vec![vec![50.0, 50.1, 49.9]],
        };
        let patterns = detect_all_patterns(&data);
        print_generate_success(Path::new("./output.yaml"), &data, &patterns);
    }

    #[test]
    fn print_run_detection_summary_does_not_panic() {
        let data = CsvData {
            columns: vec![
                csv_reader::ColumnMeta {
                    index: 1,
                    metric_name: Some("cpu".to_string()),
                    labels: std::collections::HashMap::new(),
                },
                csv_reader::ColumnMeta {
                    index: 2,
                    metric_name: Some("mem".to_string()),
                    labels: std::collections::HashMap::new(),
                },
            ],
            values: vec![vec![50.0, 50.1, 49.9], vec![80.0, 79.5, 80.1]],
        };
        let patterns = detect_all_patterns(&data);
        print_run_detection_summary(&data, &patterns);
    }

    #[test]
    fn rule_width_matches_init_module() {
        assert_eq!(
            RULE_WIDTH, 45,
            "import RULE_WIDTH should match sonda init for visual consistency"
        );
    }
}
