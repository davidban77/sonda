//! CSV-replay log generator — replays structured log events from a CSV file.
//!
//! Rows are loaded once at construction time and produce one [`LogEvent`]
//! each. Column roles (timestamp, severity, message) are resolved from the
//! CSV header either via explicit [`LogCsvColumns`] mapping or by case-
//! insensitive name auto-discovery. Every other column becomes a `fields`
//! entry on the emitted event.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::log::{LogEvent, Severity};
use crate::model::metric::Labels;
use crate::{ConfigError, GeneratorError, SondaError};

use super::{parse_severity, LogGenerator};

/// Column-role assignment for a [`LogCsvReplayGenerator`].
///
/// `timestamp`, `severity`, and `message` map header column names to their
/// semantic role. Any header column not referenced here becomes a free-form
/// `fields` entry on the emitted [`LogEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
pub struct LogCsvColumns {
    #[cfg_attr(feature = "config", serde(default))]
    pub timestamp: Option<String>,
    #[cfg_attr(feature = "config", serde(default))]
    pub severity: Option<String>,
    #[cfg_attr(feature = "config", serde(default))]
    pub message: Option<String>,
}

/// Resolved column-role indices, derived from the CSV header plus optional
/// user-supplied [`LogCsvColumns`] overrides.
#[derive(Debug, Clone)]
struct ResolvedRoles {
    severity_idx: Option<usize>,
    message_idx: Option<usize>,
    field_names: Vec<(usize, String)>,
}

/// A log generator that replays structured log events from a CSV file.
///
/// Pre-builds the full `Vec<LogEvent>` at construction; `generate(tick)`
/// returns `rows[tick % len].clone()` with zero per-tick parsing.
pub struct LogCsvReplayGenerator {
    rows: Vec<LogEvent>,
    repeat: bool,
}

impl LogCsvReplayGenerator {
    /// Construct from a file path.
    pub fn new(
        path: &str,
        columns: Option<&LogCsvColumns>,
        default_severity: Severity,
        repeat: bool,
    ) -> Result<Self, SondaError> {
        let content = std::fs::read_to_string(Path::new(path)).map_err(|e| {
            SondaError::Generator(GeneratorError::FileRead {
                path: path.to_string(),
                source: e,
            })
        })?;
        Self::from_str(&content, columns, default_severity, repeat)
    }

    /// Construct from in-memory CSV content.
    pub fn from_str(
        content: &str,
        columns: Option<&LogCsvColumns>,
        default_severity: Severity,
        repeat: bool,
    ) -> Result<Self, SondaError> {
        Self::from_str_with_fallback_count(content, columns, default_severity, repeat)
            .map(|(gen, _)| gen)
    }

    /// Same as [`from_str`](Self::from_str) but also returns the count of rows
    /// that fell back to `default_severity` due to a missing or unparseable
    /// severity cell.
    pub fn from_str_with_fallback_count(
        content: &str,
        columns: Option<&LogCsvColumns>,
        default_severity: Severity,
        repeat: bool,
    ) -> Result<(Self, usize), SondaError> {
        let lines: Vec<&str> = content
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .collect();

        if lines.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "log_csv_replay: CSV content is empty",
            )));
        }

        let header_fields: Vec<String> = super::csv_header::split_csv_header_fields(lines[0]);
        if header_fields.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "log_csv_replay: CSV header has no columns",
            )));
        }

        let roles = resolve_roles(&header_fields, columns)?;

        let data_lines = &lines[1..];
        if data_lines.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "log_csv_replay: CSV has fewer than 2 data rows (header only)",
            )));
        }

        let mut rows: Vec<LogEvent> = Vec::with_capacity(data_lines.len());
        let (_built, fallback_count) = build_rows(data_lines, &roles, default_severity, &mut rows)?;

        Ok((Self { rows, repeat }, fallback_count))
    }
}

impl LogGenerator for LogCsvReplayGenerator {
    fn generate(&self, tick: u64) -> LogEvent {
        let len = self.rows.len();
        let index = if self.repeat {
            (tick % len as u64) as usize
        } else {
            (tick.min((len - 1) as u64)) as usize
        };
        self.rows[index].clone()
    }
}

fn resolve_roles(
    header_fields: &[String],
    user_columns: Option<&LogCsvColumns>,
) -> Result<ResolvedRoles, SondaError> {
    let mut ts_idx: Option<usize> = None;
    let mut sev_idx: Option<usize> = None;
    let mut msg_idx: Option<usize> = None;

    if let Some(cfg) = user_columns {
        if let Some(ref name) = cfg.timestamp {
            ts_idx = Some(find_header(header_fields, name)?);
        }
        if let Some(ref name) = cfg.severity {
            sev_idx = Some(find_header(header_fields, name)?);
        }
        if let Some(ref name) = cfg.message {
            msg_idx = Some(find_header(header_fields, name)?);
        }
    }

    if ts_idx.is_none() {
        ts_idx = auto_match(header_fields, &["timestamp", "ts", "time"]);
    }
    if sev_idx.is_none() {
        sev_idx = auto_match(header_fields, &["severity", "level"]);
    }
    if msg_idx.is_none() {
        msg_idx = auto_match(header_fields, &["message", "msg", "log"]);
    }

    let mut field_names: Vec<(usize, String)> = Vec::new();
    for (i, name) in header_fields.iter().enumerate() {
        if Some(i) == ts_idx || Some(i) == sev_idx || Some(i) == msg_idx {
            continue;
        }
        field_names.push((i, name.clone()));
    }

    Ok(ResolvedRoles {
        severity_idx: sev_idx,
        message_idx: msg_idx,
        field_names,
    })
}

fn find_header(header_fields: &[String], name: &str) -> Result<usize, SondaError> {
    header_fields
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "log_csv_replay: column {:?} not found in CSV header",
                name
            )))
        })
}

fn auto_match(header_fields: &[String], candidates: &[&str]) -> Option<usize> {
    for (i, h) in header_fields.iter().enumerate() {
        let trimmed = h.trim();
        if candidates.iter().any(|c| trimmed.eq_ignore_ascii_case(c)) {
            return Some(i);
        }
    }
    None
}

/// Resolve the CSV column index that carries timestamps.
///
/// Reads only the first line of `path`, treats it as the CSV header, and
/// applies the same role-resolution logic the generator uses: explicit
/// `columns.timestamp` first, then case-insensitive auto-discovery against
/// `timestamp` / `ts` / `time`. Returns an error if no timestamp column can
/// be identified.
pub(crate) fn resolve_timestamp_column_index(
    path: &str,
    columns: Option<&LogCsvColumns>,
) -> Result<usize, SondaError> {
    let content = std::fs::read_to_string(Path::new(path)).map_err(|e| {
        SondaError::Generator(GeneratorError::FileRead {
            path: path.to_string(),
            source: e,
        })
    })?;
    let header_line = content
        .lines()
        .find(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "log_csv_replay: file {:?} has no header line",
                path
            )))
        })?;
    let header_fields: Vec<String> = header_line.split(',').map(|s| s.to_string()).collect();

    if let Some(cfg) = columns {
        if let Some(ref name) = cfg.timestamp {
            return find_header(&header_fields, name);
        }
    }
    auto_match(&header_fields, &["timestamp", "ts", "time"]).ok_or_else(|| {
        SondaError::Config(ConfigError::invalid(format!(
            "log_csv_replay: no timestamp column found in CSV header of {:?} \
             (expected one of: timestamp, ts, time, or an explicit 'columns.timestamp' mapping)",
            path
        )))
    })
}

fn build_rows(
    data_lines: &[&str],
    roles: &ResolvedRoles,
    default_severity: Severity,
    out: &mut Vec<LogEvent>,
) -> Result<(usize, usize), SondaError> {
    use std::time::SystemTime;

    let mut fallback_count = 0usize;
    for line in data_lines {
        let cells: Vec<String> = super::csv_header::split_csv_header_fields(line);

        let severity = match roles.severity_idx {
            Some(idx) => match cells.get(idx).map(|s| s.trim()) {
                Some(s) if !s.is_empty() => match parse_severity(s) {
                    Ok(sev) => sev,
                    Err(_) => {
                        fallback_count += 1;
                        default_severity
                    }
                },
                _ => {
                    fallback_count += 1;
                    default_severity
                }
            },
            None => default_severity,
        };

        let message = match roles.message_idx {
            Some(idx) => cells.get(idx).cloned().unwrap_or_default(),
            None => String::new(),
        };

        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for (idx, name) in &roles.field_names {
            if let Some(cell) = cells.get(*idx) {
                let trimmed = cell.trim();
                if !trimmed.is_empty() {
                    fields.insert(name.clone(), trimmed.to_string());
                }
            }
        }

        out.push(LogEvent::with_timestamp(
            SystemTime::now(),
            severity,
            message,
            Labels::default(),
            fields,
        ));
    }
    Ok((out.len(), fallback_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn log_csv_replay_generator_is_send_and_sync() {
        assert_send_sync::<LogCsvReplayGenerator>();
    }

    #[test]
    fn three_rows_produce_three_distinct_events() {
        let csv = "timestamp,severity,message\n1700000000,info,first\n1700000003,warn,second\n1700000006,error,third\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("three-row CSV must load");
        let e0 = gen.generate(0);
        let e1 = gen.generate(1);
        let e2 = gen.generate(2);
        assert_eq!(e0.message, "first");
        assert_eq!(e0.severity, Severity::Info);
        assert_eq!(e1.message, "second");
        assert_eq!(e1.severity, Severity::Warn);
        assert_eq!(e2.message, "third");
        assert_eq!(e2.severity, Severity::Error);
    }

    #[test]
    fn extra_columns_become_fields() {
        let csv = "timestamp,severity,message,user_id,region\n\
                   1700000000,info,login,u1,us-east\n\
                   1700000003,info,logout,u2,eu-west\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("extra-columns CSV must load");
        let e0 = gen.generate(0);
        assert_eq!(e0.fields.get("user_id").map(String::as_str), Some("u1"));
        assert_eq!(e0.fields.get("region").map(String::as_str), Some("us-east"));
        let e1 = gen.generate(1);
        assert_eq!(e1.fields.get("user_id").map(String::as_str), Some("u2"));
        assert_eq!(e1.fields.get("region").map(String::as_str), Some("eu-west"));
    }

    #[test]
    fn empty_severity_falls_back_to_default() {
        let csv = "timestamp,severity,message\n1700000000,,row a\n1700000003,info,row b\n";
        let (gen, fallback) =
            LogCsvReplayGenerator::from_str_with_fallback_count(csv, None, Severity::Warn, true)
                .expect("CSV must load");
        assert_eq!(gen.generate(0).severity, Severity::Warn);
        assert_eq!(gen.generate(1).severity, Severity::Info);
        assert_eq!(fallback, 1, "exactly one row had empty severity");
    }

    #[test]
    fn unknown_severity_falls_back_to_default() {
        let csv = "timestamp,severity,message\n1700000000,bogus,r1\n1700000003,info,r2\n";
        let (gen, fallback) =
            LogCsvReplayGenerator::from_str_with_fallback_count(csv, None, Severity::Error, true)
                .expect("CSV must load");
        assert_eq!(gen.generate(0).severity, Severity::Error);
        assert_eq!(gen.generate(1).severity, Severity::Info);
        assert_eq!(fallback, 1);
    }

    #[test]
    fn empty_field_cell_is_omitted_from_row_map() {
        let csv = "timestamp,severity,message,user_id\n\
                   1700000000,info,r1,u1\n\
                   1700000003,info,r2,\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("CSV must load");
        let e0 = gen.generate(0);
        assert_eq!(e0.fields.get("user_id").map(String::as_str), Some("u1"));
        let e1 = gen.generate(1);
        assert!(
            !e1.fields.contains_key("user_id"),
            "empty cell must be omitted, not present as empty string"
        );
    }

    #[test]
    fn tick_wraps_at_boundary_when_repeat_true() {
        let csv =
            "timestamp,severity,message\n1700000000,info,a\n1700000003,info,b\n1700000006,info,c\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("CSV must load");
        assert_eq!(gen.generate(0).message, "a");
        assert_eq!(gen.generate(1).message, "b");
        assert_eq!(gen.generate(2).message, "c");
        assert_eq!(gen.generate(3).message, "a", "tick=3 wraps to row 0");
        let expected_idx = (u64::MAX % 3) as usize;
        assert_eq!(
            gen.generate(u64::MAX).message,
            ["a", "b", "c"][expected_idx],
            "u64::MAX % 3 = {expected_idx}"
        );
    }

    #[test]
    fn tick_clamps_to_last_when_repeat_false() {
        let csv = "timestamp,severity,message\n1700000000,info,a\n1700000003,info,b\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, false)
            .expect("CSV must load");
        assert_eq!(gen.generate(0).message, "a");
        assert_eq!(gen.generate(1).message, "b");
        assert_eq!(gen.generate(2).message, "b");
        assert_eq!(gen.generate(u64::MAX).message, "b");
    }

    #[test]
    fn empty_csv_returns_error() {
        let result = LogCsvReplayGenerator::from_str("", None, Severity::Info, true);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("empty"),
            "error must mention 'empty', got: {msg}"
        );
    }

    #[test]
    fn header_only_csv_returns_error() {
        let csv = "timestamp,severity,message\n";
        let result = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("fewer than 2 data rows"),
            "error must mention 'fewer than 2 data rows', got: {msg}"
        );
    }

    #[test]
    fn explicit_columns_override_auto_discovery() {
        let csv = "ts,sev,text\n1700000000,error,boom\n1700000003,info,ok\n";
        let columns = LogCsvColumns {
            timestamp: Some("ts".to_string()),
            severity: Some("sev".to_string()),
            message: Some("text".to_string()),
        };
        let gen = LogCsvReplayGenerator::from_str(csv, Some(&columns), Severity::Info, true)
            .expect("explicit columns must resolve");
        let e0 = gen.generate(0);
        assert_eq!(e0.message, "boom");
        assert_eq!(e0.severity, Severity::Error);
    }

    #[test]
    fn from_str_constructor_does_not_need_disk() {
        let csv = "timestamp,severity,message\n1700000000,info,r1\n1700000003,info,r2\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true);
        assert!(gen.is_ok(), "from_str must not require disk I/O");
    }

    #[test]
    fn auto_discovery_matches_case_insensitively() {
        let csv = "TIME,Level,MSG\n1700000000,WARN,r1\n1700000003,error,r2\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("case-insensitive auto-discovery must work");
        assert_eq!(gen.generate(0).severity, Severity::Warn);
        assert_eq!(gen.generate(0).message, "r1");
    }

    #[test]
    fn comment_and_blank_lines_are_skipped() {
        let csv = "# preamble\ntimestamp,severity,message\n\n1700000000,info,r1\n\n# mid-comment\n1700000003,info,r2\n";
        let gen = LogCsvReplayGenerator::from_str(csv, None, Severity::Info, true)
            .expect("comments and blanks must be skipped");
        assert_eq!(gen.generate(0).message, "r1");
        assert_eq!(gen.generate(1).message, "r2");
    }
}
