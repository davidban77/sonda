use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sonda_core::Severity;

use crate::rawlog::ParsedLogRow;
use crate::ParsersError;

const SYNTHETIC_ANCHOR_EPOCH: f64 = 1_700_000_000.0;

#[derive(Debug, Clone)]
pub struct WrittenLogCsv {
    pub path: PathBuf,
    pub row_count: usize,
    pub first_timestamp: f64,
    pub last_timestamp: f64,
}

pub fn write_log_csv(
    rows: &[ParsedLogRow],
    output_path: &Path,
    delta_seconds: f64,
) -> Result<WrittenLogCsv, ParsersError> {
    if rows.is_empty() {
        return Err(ParsersError::EmptyInput {
            path: output_path.to_path_buf(),
        });
    }

    let field_columns = collect_field_columns(rows);
    let timestamps = resolve_timestamps(rows, delta_seconds);

    let file = File::create(output_path).map_err(|e| ParsersError::OutputWrite {
        path: output_path.to_path_buf(),
        source: e,
    })?;
    let mut w = BufWriter::new(file);

    write_header(&mut w, &field_columns, output_path)?;

    for (idx, row) in rows.iter().enumerate() {
        write_row(&mut w, row, timestamps[idx], &field_columns, output_path)?;
    }

    w.flush().map_err(|e| ParsersError::OutputWrite {
        path: output_path.to_path_buf(),
        source: e,
    })?;

    Ok(WrittenLogCsv {
        path: output_path.to_path_buf(),
        row_count: rows.len(),
        first_timestamp: timestamps[0],
        last_timestamp: timestamps[timestamps.len() - 1],
    })
}

fn collect_field_columns(rows: &[ParsedLogRow]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        for key in row.fields.keys() {
            set.insert(key.clone());
        }
    }
    set.into_iter().collect()
}

fn resolve_timestamps(rows: &[ParsedLogRow], delta_seconds: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(rows.len());
    let mut next_synth = SYNTHETIC_ANCHOR_EPOCH;
    for row in rows {
        match row.timestamp {
            Some(ts) => out.push(ts),
            None => {
                out.push(next_synth);
                next_synth += delta_seconds;
            }
        }
    }
    out
}

fn write_header(
    w: &mut BufWriter<File>,
    field_columns: &[String],
    path: &Path,
) -> Result<(), ParsersError> {
    let mut line = String::from("timestamp,severity,message");
    for col in field_columns {
        line.push(',');
        line.push_str(&csv_escape(col));
    }
    line.push('\n');
    w.write_all(line.as_bytes())
        .map_err(|e| ParsersError::OutputWrite {
            path: path.to_path_buf(),
            source: e,
        })
}

fn write_row(
    w: &mut BufWriter<File>,
    row: &ParsedLogRow,
    timestamp: f64,
    field_columns: &[String],
    path: &Path,
) -> Result<(), ParsersError> {
    let mut line = String::new();
    line.push_str(&format_timestamp(timestamp));
    line.push(',');
    line.push_str(severity_cell(row.severity));
    line.push(',');
    line.push_str(&csv_escape(&row.message));
    for col in field_columns {
        line.push(',');
        if let Some(v) = row.fields.get(col) {
            line.push_str(&csv_escape(v));
        }
    }
    line.push('\n');
    w.write_all(line.as_bytes())
        .map_err(|e| ParsersError::OutputWrite {
            path: path.to_path_buf(),
            source: e,
        })
}

fn severity_cell(sev: Option<Severity>) -> &'static str {
    match sev {
        Some(Severity::Trace) => "trace",
        Some(Severity::Debug) => "debug",
        Some(Severity::Info) => "info",
        Some(Severity::Warn) => "warn",
        Some(Severity::Error) => "error",
        Some(Severity::Fatal) => "fatal",
        None => "",
    }
}

fn format_timestamp(ts: f64) -> String {
    if ts.fract() == 0.0 {
        format!("{}", ts as i64)
    } else {
        format!("{ts}")
    }
}

fn csv_escape(cell: &str) -> String {
    let needs_quoting =
        cell.contains(',') || cell.contains('"') || cell.contains('\n') || cell.contains('\r');
    if !needs_quoting {
        return cell.to_string();
    }
    let mut out = String::with_capacity(cell.len() + 2);
    out.push('"');
    for ch in cell.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[derive(Debug, Clone)]
pub struct EmitScenarioParams<'a> {
    pub scenario_name: &'a str,
    pub csv_path: &'a Path,
    pub yaml_path: &'a Path,
    pub first_timestamp: f64,
    pub last_timestamp: f64,
    pub row_count: usize,
    pub delta_seconds: f64,
    pub synthesized_timestamps: bool,
}

pub fn emit_scenario_yaml(params: EmitScenarioParams<'_>) -> Result<PathBuf, ParsersError> {
    let yaml_parent = params
        .yaml_path
        .parent()
        .ok_or_else(|| ParsersError::OutputHasNoParent {
            path: params.yaml_path.to_path_buf(),
        })?;
    let csv_rel = relative_path(params.csv_path, yaml_parent);
    let csv_rel_str = csv_rel.to_string_lossy().into_owned();

    let duration_seconds = if params.synthesized_timestamps {
        (params.row_count as f64 * params.delta_seconds).ceil() as u64
    } else {
        (params.last_timestamp - params.first_timestamp)
            .ceil()
            .max(1.0) as u64
    };
    let duration = format!("{duration_seconds}s");

    let scenario = ScenarioFile {
        version: 2,
        defaults: Defaults {
            rate: 1,
            duration,
            encoder: EncoderBlock {
                kind: "json_lines".to_string(),
            },
            sink: SinkBlock {
                kind: "stdout".to_string(),
            },
        },
        scenarios: vec![ScenarioBlock {
            signal_type: "logs".to_string(),
            name: params.scenario_name.to_string(),
            log_generator: LogGeneratorBlock {
                kind: "csv_replay".to_string(),
                file: csv_rel_str,
                default_severity: "info".to_string(),
                repeat: false,
            },
        }],
    };

    let yaml = serde_yaml_ng::to_string(&scenario)?;
    std::fs::write(params.yaml_path, yaml).map_err(|e| ParsersError::OutputWrite {
        path: params.yaml_path.to_path_buf(),
        source: e,
    })?;

    Ok(params.yaml_path.to_path_buf())
}

fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let target_abs = absolutize(target);
    let base_abs = absolutize(base);

    let target_components: Vec<Component<'_>> = target_abs.components().collect();
    let base_components: Vec<Component<'_>> = base_abs.components().collect();

    let mut common = 0usize;
    while common < target_components.len()
        && common < base_components.len()
        && target_components[common] == base_components[common]
    {
        common += 1;
    }

    let mut out = PathBuf::new();
    for _ in 0..(base_components.len() - common) {
        out.push("..");
    }
    for comp in &target_components[common..] {
        out.push(comp.as_os_str());
    }

    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[derive(Serialize)]
struct ScenarioFile {
    version: u32,
    defaults: Defaults,
    scenarios: Vec<ScenarioBlock>,
}

#[derive(Serialize)]
struct Defaults {
    rate: u32,
    duration: String,
    encoder: EncoderBlock,
    sink: SinkBlock,
}

#[derive(Serialize)]
struct EncoderBlock {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
struct SinkBlock {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
struct ScenarioBlock {
    signal_type: String,
    name: String,
    log_generator: LogGeneratorBlock,
}

#[derive(Serialize)]
struct LogGeneratorBlock {
    #[serde(rename = "type")]
    kind: String,
    file: String,
    default_severity: String,
    repeat: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn row(ts: Option<f64>, sev: Option<Severity>, msg: &str) -> ParsedLogRow {
        ParsedLogRow {
            timestamp: ts,
            severity: sev,
            message: msg.to_string(),
            fields: BTreeMap::new(),
        }
    }

    fn row_with_fields(
        ts: Option<f64>,
        sev: Option<Severity>,
        msg: &str,
        fields: &[(&str, &str)],
    ) -> ParsedLogRow {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert((*k).to_string(), (*v).to_string());
        }
        ParsedLogRow {
            timestamp: ts,
            severity: sev,
            message: msg.to_string(),
            fields: map,
        }
    }

    #[test]
    fn writer_emits_header_in_documented_column_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![row_with_fields(
            Some(1.0),
            Some(Severity::Info),
            "hi",
            &[("zeta", "z"), ("alpha", "a")],
        )];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let first_line = content.lines().next().unwrap();
        assert_eq!(first_line, "timestamp,severity,message,alpha,zeta");
    }

    #[test]
    fn writer_sorts_field_columns_alphabetically() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![
            row_with_fields(Some(1.0), Some(Severity::Info), "a", &[("kappa", "k")]),
            row_with_fields(Some(2.0), Some(Severity::Info), "b", &[("beta", "b")]),
            row_with_fields(Some(3.0), Some(Severity::Info), "c", &[("alpha", "a")]),
        ];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let header = content.lines().next().unwrap();
        assert_eq!(header, "timestamp,severity,message,alpha,beta,kappa");
    }

    #[test]
    fn writer_emits_empty_severity_as_empty_cell() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![row(Some(1.0), None, "no severity")];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[1], "1,,no severity");
    }

    #[test]
    fn writer_quotes_cells_with_commas() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![row(Some(1.0), Some(Severity::Info), "hello, world")];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[1], r#"1,info,"hello, world""#);
    }

    #[test]
    fn writer_doubles_internal_double_quotes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![row(Some(1.0), Some(Severity::Info), r#"she said "hi""#)];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[1], r#"1,info,"she said ""hi""""#);
    }

    #[test]
    fn writer_synthesizes_timestamps_at_anchor_with_delta() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![
            row(None, Some(Severity::Info), "a"),
            row(None, Some(Severity::Info), "b"),
            row(None, Some(Severity::Info), "c"),
        ];
        let written = write_log_csv(&rows, &path, 2.0).unwrap();
        assert_eq!(written.first_timestamp, 1_700_000_000.0);
        assert_eq!(written.last_timestamp, 1_700_000_004.0);
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[1], "1700000000,info,a");
        assert_eq!(lines[2], "1700000002,info,b");
        assert_eq!(lines[3], "1700000004,info,c");
    }

    #[test]
    fn writer_preserves_real_timestamps() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![
            row(Some(1_700_000_100.0), Some(Severity::Info), "a"),
            row(Some(1_700_000_200.0), Some(Severity::Warn), "b"),
        ];
        let written = write_log_csv(&rows, &path, 1.0).unwrap();
        assert_eq!(written.first_timestamp, 1_700_000_100.0);
        assert_eq!(written.last_timestamp, 1_700_000_200.0);
    }

    #[test]
    fn writer_returns_empty_input_on_zero_rows() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let err = write_log_csv(&[], &path, 1.0).unwrap_err();
        assert!(matches!(err, ParsersError::EmptyInput { .. }));
    }

    #[test]
    fn writer_omits_field_cell_for_row_missing_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv");
        let rows = vec![
            row_with_fields(Some(1.0), Some(Severity::Info), "a", &[("k", "v")]),
            row(Some(2.0), Some(Severity::Info), "b"),
        ];
        write_log_csv(&rows, &path, 1.0).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "timestamp,severity,message,k");
        assert_eq!(lines[1], "1,info,a,v");
        assert_eq!(lines[2], "2,info,b,");
    }

    #[test]
    fn emit_scenario_yaml_uses_relative_csv_path() {
        let dir = TempDir::new().unwrap();
        let csv_path = dir.path().join("foo.csv");
        let yaml_path = dir.path().join("foo.yaml");
        std::fs::write(&csv_path, "timestamp,severity,message\n1,info,x\n").unwrap();

        let params = EmitScenarioParams {
            scenario_name: "test_replay",
            csv_path: &csv_path,
            yaml_path: &yaml_path,
            first_timestamp: 1.0,
            last_timestamp: 5.0,
            row_count: 5,
            delta_seconds: 1.0,
            synthesized_timestamps: false,
        };
        emit_scenario_yaml(params).unwrap();
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        assert!(
            yaml.contains("file: foo.csv"),
            "expected relative csv path in yaml, got: {yaml}"
        );
        assert!(yaml.contains("name: test_replay"));
        assert!(yaml.contains("default_severity: info"));
    }

    #[test]
    fn emit_scenario_yaml_uses_synthesized_duration_when_flag_set() {
        let dir = TempDir::new().unwrap();
        let csv_path = dir.path().join("p.csv");
        let yaml_path = dir.path().join("p.yaml");

        let params = EmitScenarioParams {
            scenario_name: "plain_replay",
            csv_path: &csv_path,
            yaml_path: &yaml_path,
            first_timestamp: 1_700_000_000.0,
            last_timestamp: 1_700_000_004.0,
            row_count: 3,
            delta_seconds: 2.0,
            synthesized_timestamps: true,
        };
        emit_scenario_yaml(params).unwrap();
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        assert!(yaml.contains("duration: 6s"), "got: {yaml}");
    }

    #[test]
    fn relative_path_handles_sibling_files() {
        let target = Path::new("/tmp/a/foo.csv");
        let base = Path::new("/tmp/a");
        assert_eq!(relative_path(target, base), PathBuf::from("foo.csv"));
    }

    #[test]
    fn relative_path_handles_subdirectory_target() {
        let target = Path::new("/tmp/a/sub/foo.csv");
        let base = Path::new("/tmp/a");
        assert_eq!(relative_path(target, base), PathBuf::from("sub/foo.csv"));
    }

    #[test]
    fn relative_path_handles_parent_target() {
        let target = Path::new("/tmp/foo.csv");
        let base = Path::new("/tmp/a");
        assert_eq!(relative_path(target, base), PathBuf::from("../foo.csv"));
    }
}
