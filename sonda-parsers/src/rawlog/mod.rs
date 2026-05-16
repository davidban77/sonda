pub mod formats;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sonda_core::Severity;

use crate::canonical::{emit_scenario_yaml, write_log_csv, EmitScenarioParams};
use crate::ParsersError;

#[derive(Debug, Clone)]
pub struct ParsedLogRow {
    pub timestamp: Option<f64>,
    pub severity: Option<Severity>,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

pub trait LogFormatParser: Send + Sync {
    fn name(&self) -> &'static str;

    fn parse_line(&self, line: &str) -> Option<ParsedLogRow>;

    /// Called once after all lines have been consumed. Default is a no-op;
    /// parsers that accumulate state across `parse_line` calls (e.g. for
    /// aggregated warnings) override this to flush.
    fn finalize(&self) {}
}

#[derive(Debug, Clone)]
pub struct RawlogArgs {
    pub input: PathBuf,
    pub format: String,
    pub output: PathBuf,
    pub delta_seconds: Option<f64>,
    pub scenario_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawlogOutput {
    pub csv_path: PathBuf,
    pub yaml_path: PathBuf,
    pub row_count: usize,
    pub format: &'static str,
}

pub fn run(args: RawlogArgs) -> Result<RawlogOutput, ParsersError> {
    let delta = args.delta_seconds.unwrap_or(1.0);
    if !delta.is_finite() || delta <= 0.0 {
        return Err(ParsersError::InvalidDelta { value: delta });
    }

    let parsers = formats::all_parsers();
    let parser = lookup_parser(&parsers, &args.format)?;

    let content = std::fs::read_to_string(&args.input).map_err(|e| ParsersError::InputRead {
        path: args.input.clone(),
        source: e,
    })?;

    let mut rows: Vec<ParsedLogRow> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(row) = parser.parse_line(trimmed) {
            rows.push(row);
        }
    }
    parser.finalize();

    if rows.is_empty() {
        return Err(ParsersError::EmptyInput {
            path: args.input.clone(),
        });
    }

    let synthesized = rows.iter().all(|r| r.timestamp.is_none());

    let csv_path = derive_csv_path(&args.input, &args.output);
    let written = write_log_csv(&rows, &csv_path, delta)?;

    let scenario_name = args
        .scenario_name
        .clone()
        .unwrap_or_else(|| default_scenario_name(&args.input));

    let yaml_path = emit_scenario_yaml(EmitScenarioParams {
        scenario_name: &scenario_name,
        csv_path: &written.path,
        yaml_path: &args.output,
        first_timestamp: written.first_timestamp,
        last_timestamp: written.last_timestamp,
        row_count: written.row_count,
        delta_seconds: delta,
        synthesized_timestamps: synthesized,
    })?;

    Ok(RawlogOutput {
        csv_path: written.path,
        yaml_path,
        row_count: written.row_count,
        format: parser.name(),
    })
}

fn lookup_parser<'a>(
    parsers: &'a [Box<dyn LogFormatParser>],
    name: &str,
) -> Result<&'a dyn LogFormatParser, ParsersError> {
    for p in parsers {
        if p.name() == name {
            return Ok(p.as_ref());
        }
    }
    Err(ParsersError::UnknownFormat {
        name: name.to_string(),
        known: parsers.iter().map(|p| p.name()).collect(),
    })
}

fn derive_csv_path(input_path: &Path, yaml_path: &Path) -> PathBuf {
    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rawlog");
    let parent = yaml_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.csv"))
}

fn default_scenario_name(input: &Path) -> String {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rawlog");
    let safe: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{safe}_replay")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_input(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn unknown_format_returns_unknown_format_error() {
        let dir = TempDir::new().unwrap();
        let input = write_input(&dir, "in.log", "hello\n");
        let yaml = dir.path().join("out.yaml");
        let err = run(RawlogArgs {
            input,
            format: "doesnotexist".to_string(),
            output: yaml,
            delta_seconds: None,
            scenario_name: None,
        })
        .unwrap_err();
        match err {
            ParsersError::UnknownFormat { name, known } => {
                assert_eq!(name, "doesnotexist");
                assert!(known.contains(&"plain"));
                assert!(known.contains(&"nginx"));
            }
            other => panic!("expected UnknownFormat, got: {other:?}"),
        }
    }

    #[test]
    fn empty_file_returns_empty_input_error() {
        let dir = TempDir::new().unwrap();
        let input = write_input(&dir, "in.log", "   \n\n\n");
        let yaml = dir.path().join("out.yaml");
        let err = run(RawlogArgs {
            input,
            format: "plain".to_string(),
            output: yaml,
            delta_seconds: None,
            scenario_name: None,
        })
        .unwrap_err();
        assert!(matches!(err, ParsersError::EmptyInput { .. }));
    }

    #[test]
    fn run_emits_csv_and_yaml_for_plain_format() {
        let dir = TempDir::new().unwrap();
        let input = write_input(&dir, "app.log", "line one\nline two\nline three\n");
        let yaml = dir.path().join("out.yaml");
        let output = run(RawlogArgs {
            input,
            format: "plain".to_string(),
            output: yaml.clone(),
            delta_seconds: Some(2.0),
            scenario_name: Some("custom_name".to_string()),
        })
        .unwrap();

        assert_eq!(output.row_count, 3);
        assert_eq!(output.format, "plain");
        assert!(output.csv_path.exists());
        assert!(output.yaml_path.exists());

        let csv = std::fs::read_to_string(&output.csv_path).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "timestamp,severity,message");
        assert_eq!(lines[1], "1700000000,,line one");
        assert_eq!(lines[2], "1700000002,,line two");
        assert_eq!(lines[3], "1700000004,,line three");

        let yaml_text = std::fs::read_to_string(&output.yaml_path).unwrap();
        assert!(yaml_text.contains("name: custom_name"));
    }

    #[test]
    fn default_scenario_name_derives_from_file_stem() {
        assert_eq!(
            default_scenario_name(Path::new("/tmp/sample-nginx.log")),
            "sample_nginx_replay"
        );
        assert_eq!(
            default_scenario_name(Path::new("foo.bar.log")),
            "foo_bar_replay"
        );
    }

    #[test]
    fn invalid_delta_seconds_returns_error() {
        for bad in [0.0_f64, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let dir = TempDir::new().unwrap();
            let input = write_input(&dir, "in.log", "hello\n");
            let yaml = dir.path().join("out.yaml");
            let err = run(RawlogArgs {
                input,
                format: "plain".to_string(),
                output: yaml,
                delta_seconds: Some(bad),
                scenario_name: None,
            })
            .unwrap_err();
            assert!(
                matches!(err, ParsersError::InvalidDelta { value } if value.to_bits() == bad.to_bits()),
                "delta_seconds={bad} must be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn derive_csv_path_uses_input_stem_in_yaml_parent_dir() {
        assert_eq!(
            derive_csv_path(
                Path::new("/data/sample-nginx.log"),
                Path::new("/tmp/out.yaml")
            ),
            PathBuf::from("/tmp/sample-nginx.csv")
        );
        assert_eq!(
            derive_csv_path(Path::new("foo.bar.log"), Path::new("subdir/out.yml")),
            PathBuf::from("subdir/foo.bar.csv")
        );
    }
}
