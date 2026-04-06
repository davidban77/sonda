//! CSV header parsing for Grafana-style label-aware column headers.
//!
//! This module provides pure parsing functions (no I/O) for extracting metric
//! names and label key-value pairs from CSV column headers. It supports the
//! five header formats produced by Grafana's "Series joined by time" CSV export
//! and plain metric names used in hand-authored CSV files.
//!
//! # Supported header formats
//!
//! 1. `{__name__="up", instance="foo", job="bar"}` -- name from `__name__`, labels extracted.
//! 2. `up{instance="foo", job="bar"}` -- name before `{`, labels extracted.
//! 3. `{instance="foo", job="bar"}` -- no name, labels only.
//! 4. `cpu_percent` -- plain name, no labels.
//! 5. `prometheus` -- plain name, no labels.
//!
//! # Usage
//!
//! These functions are called once at config expansion time to auto-discover
//! column metadata from a CSV file header. They are not on the hot path.

use std::collections::HashMap;

use crate::{ConfigError, SondaError};

/// Result of parsing a single CSV column header.
///
/// Contains the metric name (if present) and any label key-value pairs
/// extracted from `{key="value", ...}` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedColumnHeader {
    /// Metric name extracted from the header. `None` when the header has
    /// labels only (format 3: `{instance="foo", job="bar"}`).
    pub metric_name: Option<String>,
    /// Label key-value pairs from `{key="value", ...}` syntax. Empty for
    /// plain names (formats 4 and 5).
    pub labels: HashMap<String, String>,
}

/// Parse a single CSV column header into a metric name and label set.
///
/// Recognizes five formats:
///
/// 1. `{__name__="up", instance="foo", job="bar"}` -- name: `Some("up")`, labels: `{instance, job}`
/// 2. `up{instance="foo", job="bar"}` -- name: `Some("up")`, labels: `{instance, job}`
/// 3. `{instance="foo", job="bar"}` -- name: `None`, labels: `{instance, job}`
/// 4. `cpu_percent` -- name: `Some("cpu_percent")`, labels: empty
/// 5. `prometheus` -- name: `Some("prometheus")`, labels: empty
///
/// For format 1, `__name__` is extracted as the metric name and removed from
/// the returned label set.
///
/// The caller must pass an already-unquoted header (CSV `""` replaced with `"`).
///
/// # Errors
///
/// Returns [`SondaError::Config`] for malformed header syntax (unmatched
/// braces, missing `=`, unterminated quoted values, etc.).
pub(crate) fn parse_column_header(header: &str) -> Result<ParsedColumnHeader, SondaError> {
    let header = header.trim();

    if header.is_empty() {
        return Ok(ParsedColumnHeader {
            metric_name: None,
            labels: HashMap::new(),
        });
    }

    // Find the first `{`.
    let brace_pos = header.find('{');

    match brace_pos {
        None => {
            // Format 4 or 5: plain name, no labels.
            Ok(ParsedColumnHeader {
                metric_name: Some(header.to_string()),
                labels: HashMap::new(),
            })
        }
        Some(0) => {
            // Format 1 or 3: starts with `{`.
            let mut labels = parse_label_block(header)?;
            let name = labels.remove("__name__");
            Ok(ParsedColumnHeader {
                metric_name: name,
                labels,
            })
        }
        Some(pos) => {
            // Format 2: name before `{`.
            let name = header[..pos].trim().to_string();
            let labels = parse_label_block(&header[pos..])?;
            Ok(ParsedColumnHeader {
                metric_name: Some(name),
                labels,
            })
        }
    }
}

/// Parse a label block of the form `{key="value", key="value", ...}`.
///
/// The input must start with `{` and contain a matching `}`. Returns the
/// parsed key-value pairs as a `HashMap`.
///
/// # Errors
///
/// Returns [`SondaError::Config`] for:
/// - Missing closing `}`
/// - Missing `=` between key and value
/// - Unterminated quoted values
fn parse_label_block(block: &str) -> Result<HashMap<String, String>, SondaError> {
    let block = block.trim();

    if !block.starts_with('{') {
        return Err(SondaError::Config(ConfigError::invalid(
            "csv_header: label block must start with '{'",
        )));
    }

    // rfind is safe here: any '}' inside a label value is enclosed in
    // quotes within the `{...}` block, so it structurally precedes the
    // real closing '}' which is always the last one in the string.
    let close = block.rfind('}').ok_or_else(|| {
        SondaError::Config(ConfigError::invalid(
            "csv_header: unmatched '{' — missing closing '}'",
        ))
    })?;

    let inner = block[1..close].trim();
    if inner.is_empty() {
        return Ok(HashMap::new());
    }

    parse_label_pairs(inner)
}

/// Parse comma-separated `key="value"` pairs from the interior of a label block.
///
/// Handles:
/// - Whitespace around `,`, `=`, keys, and values.
/// - Quoted values with escaped quotes (`\"` inside values).
/// - Unquoted values (trimmed of whitespace).
fn parse_label_pairs(inner: &str) -> Result<HashMap<String, String>, SondaError> {
    let mut labels = HashMap::new();
    let mut remaining = inner.trim();

    while !remaining.is_empty() {
        // Find the `=` separator.
        let eq_pos = remaining.find('=').ok_or_else(|| {
            SondaError::Config(ConfigError::invalid(format!(
                "csv_header: expected '=' in label pair, got: {:?}",
                remaining
            )))
        })?;

        let key = remaining[..eq_pos].trim();
        if key.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(
                "csv_header: empty label key",
            )));
        }

        remaining = remaining[eq_pos + 1..].trim_start();

        // Parse the value.
        let (value, rest) = if let Some(stripped) = remaining.strip_prefix('"') {
            parse_quoted_value(stripped)?
        } else {
            parse_unquoted_value(remaining)?
        };

        labels.insert(key.to_string(), value);
        remaining = rest.trim_start();

        // Consume comma separator if present.
        if remaining.starts_with(',') {
            remaining = remaining[1..].trim_start();
        }
    }

    Ok(labels)
}

/// Parse a quoted value starting after the opening `"`.
///
/// Returns the unescaped value and the remaining unparsed input (after the
/// closing `"` and any trailing whitespace).
fn parse_quoted_value(input: &str) -> Result<(String, &str), SondaError> {
    let mut value = String::new();
    let mut chars = input.char_indices();

    loop {
        match chars.next() {
            None => {
                return Err(SondaError::Config(ConfigError::invalid(
                    "csv_header: unterminated quoted value",
                )));
            }
            Some((_, '\\')) => {
                // Escaped character.
                if let Some((_, ch)) = chars.next() {
                    value.push(ch);
                } else {
                    return Err(SondaError::Config(ConfigError::invalid(
                        "csv_header: unterminated escape in quoted value",
                    )));
                }
            }
            Some((i, '"')) => {
                // End of quoted value.
                let rest = &input[i + 1..];
                return Ok((value, rest));
            }
            Some((_, ch)) => {
                value.push(ch);
            }
        }
    }
}

/// Parse an unquoted value (terminated by `,` or end of input).
///
/// Returns the trimmed value and the remaining unparsed input.
fn parse_unquoted_value(input: &str) -> Result<(String, &str), SondaError> {
    match input.find(',') {
        Some(pos) => {
            let value = input[..pos].trim().to_string();
            Ok((value, &input[pos..]))
        }
        None => {
            let value = input.trim().to_string();
            Ok((value, ""))
        }
    }
}

/// Detect whether a CSV line is a header row by checking if any
/// non-first field fails to parse as `f64`.
///
/// A line is considered a header when any field after the first column
/// (index > 0) cannot be parsed as `f64`. For single-column CSVs, the
/// line is a header if the sole field cannot be parsed as `f64`.
///
/// This function uses naive `split(',')` rather than RFC 4180-aware
/// parsing. For Grafana exports with quoted fields, the split produces
/// more fragments than actual columns, but every fragment of a
/// non-numeric header is itself non-numeric, so the heuristic still
/// correctly identifies headers. A false positive would require a quoted
/// numeric value like `"1000"` — extremely unlikely in practice.
pub(crate) fn is_header_line(line: &str) -> bool {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() <= 1 {
        // Single-column: header if the field is non-numeric.
        return fields
            .first()
            .map(|f| f.trim().parse::<f64>().is_err())
            .unwrap_or(false);
    }
    // Multi-column: header if any non-time field (index > 0) is non-numeric.
    fields
        .iter()
        .skip(1)
        .any(|f| f.trim().parse::<f64>().is_err())
}

/// Split a CSV header line into fields respecting RFC 4180 quoting.
///
/// Strips outer quotes from each field and replaces `""` (RFC 4180 escaped
/// quotes) with `"`. This function is used only for header parsing at load
/// time and is not on the hot path.
pub(crate) fn split_csv_header_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    // RFC 4180 escaped quote: `""` -> `"`
                    current.push('"');
                    chars.next();
                } else {
                    // End of quoted field.
                    in_quotes = false;
                }
            } else {
                current.push(ch);
            }
        } else {
            match ch {
                ',' => {
                    fields.push(current.clone());
                    current.clear();
                }
                '"' => {
                    in_quotes = true;
                }
                _ => {
                    current.push(ch);
                }
            }
        }
    }

    fields.push(current);
    fields
}

/// Parse all column headers from a CSV header line.
///
/// Splits the line into fields using [`split_csv_header_fields`], then parses
/// each field with [`parse_column_header`]. Returns one [`ParsedColumnHeader`]
/// per column (including column 0, which is typically a timestamp).
///
/// # Errors
///
/// Returns [`SondaError::Config`] if any column header has malformed syntax.
pub fn parse_header_row(line: &str) -> Result<Vec<ParsedColumnHeader>, SondaError> {
    let fields = split_csv_header_fields(line);
    fields
        .iter()
        .map(|field| parse_column_header(field))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_column_header: Format 1 — {__name__="metric", labels...}
    // -----------------------------------------------------------------------

    #[test]
    fn format1_name_from_dunder_name_label() {
        let h =
            parse_column_header(r#"{__name__="up", instance="localhost:9090", job="prometheus"}"#)
                .expect("format 1 must parse");
        assert_eq!(h.metric_name.as_deref(), Some("up"));
        assert_eq!(
            h.labels.get("instance").map(|s| s.as_str()),
            Some("localhost:9090")
        );
        assert_eq!(h.labels.get("job").map(|s| s.as_str()), Some("prometheus"));
        assert!(
            !h.labels.contains_key("__name__"),
            "__name__ must be removed from labels"
        );
    }

    #[test]
    fn format1_name_only_in_dunder() {
        let h = parse_column_header(r#"{__name__="process_cpu_seconds_total"}"#)
            .expect("single __name__ must parse");
        assert_eq!(h.metric_name.as_deref(), Some("process_cpu_seconds_total"));
        assert!(h.labels.is_empty());
    }

    // -----------------------------------------------------------------------
    // parse_column_header: Format 2 — name{labels...}
    // -----------------------------------------------------------------------

    #[test]
    fn format2_name_before_brace() {
        let h = parse_column_header(r#"up{instance="localhost:9090", job="prometheus"}"#)
            .expect("format 2 must parse");
        assert_eq!(h.metric_name.as_deref(), Some("up"));
        assert_eq!(
            h.labels.get("instance").map(|s| s.as_str()),
            Some("localhost:9090")
        );
        assert_eq!(h.labels.get("job").map(|s| s.as_str()), Some("prometheus"));
    }

    #[test]
    fn format2_name_with_empty_labels() {
        let h = parse_column_header("up{}").expect("empty braces must parse");
        assert_eq!(h.metric_name.as_deref(), Some("up"));
        assert!(h.labels.is_empty());
    }

    #[test]
    fn format2_name_with_single_label() {
        let h = parse_column_header(r#"http_requests_total{method="GET"}"#)
            .expect("single label must parse");
        assert_eq!(h.metric_name.as_deref(), Some("http_requests_total"));
        assert_eq!(h.labels.len(), 1);
        assert_eq!(h.labels.get("method").map(|s| s.as_str()), Some("GET"));
    }

    // -----------------------------------------------------------------------
    // parse_column_header: Format 3 — {labels only, no __name__}
    // -----------------------------------------------------------------------

    #[test]
    fn format3_labels_only_no_name() {
        let h = parse_column_header(r#"{instance="foo", job="bar"}"#).expect("format 3 must parse");
        assert!(h.metric_name.is_none(), "format 3 must have no metric name");
        assert_eq!(h.labels.get("instance").map(|s| s.as_str()), Some("foo"));
        assert_eq!(h.labels.get("job").map(|s| s.as_str()), Some("bar"));
    }

    // -----------------------------------------------------------------------
    // parse_column_header: Format 4/5 — plain names
    // -----------------------------------------------------------------------

    #[test]
    fn format4_plain_name() {
        let h = parse_column_header("cpu_percent").expect("plain name must parse");
        assert_eq!(h.metric_name.as_deref(), Some("cpu_percent"));
        assert!(h.labels.is_empty());
    }

    #[test]
    fn format5_simple_word() {
        let h = parse_column_header("prometheus").expect("simple word must parse");
        assert_eq!(h.metric_name.as_deref(), Some("prometheus"));
        assert!(h.labels.is_empty());
    }

    #[test]
    fn plain_name_with_whitespace() {
        let h = parse_column_header("  cpu_percent  ").expect("trimmed plain name must parse");
        assert_eq!(h.metric_name.as_deref(), Some("cpu_percent"));
        assert!(h.labels.is_empty());
    }

    // -----------------------------------------------------------------------
    // parse_column_header: edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn empty_header_returns_no_name_no_labels() {
        let h = parse_column_header("").expect("empty header must parse");
        assert!(h.metric_name.is_none());
        assert!(h.labels.is_empty());
    }

    #[test]
    fn whitespace_only_header() {
        let h = parse_column_header("   ").expect("whitespace header must parse");
        assert!(h.metric_name.is_none());
        assert!(h.labels.is_empty());
    }

    #[test]
    fn empty_braces() {
        let h = parse_column_header("{}").expect("empty braces must parse");
        assert!(h.metric_name.is_none());
        assert!(h.labels.is_empty());
    }

    #[test]
    fn spaces_around_label_pairs() {
        let h = parse_column_header(r#"{ instance = "foo" , job = "bar" }"#)
            .expect("spaces around pairs must parse");
        assert!(h.metric_name.is_none());
        assert_eq!(h.labels.get("instance").map(|s| s.as_str()), Some("foo"));
        assert_eq!(h.labels.get("job").map(|s| s.as_str()), Some("bar"));
    }

    #[test]
    fn label_value_with_escaped_quote() {
        let h =
            parse_column_header(r#"{path="say \"hello\""}"#).expect("escaped quotes must parse");
        assert_eq!(
            h.labels.get("path").map(|s| s.as_str()),
            Some(r#"say "hello""#)
        );
    }

    #[test]
    fn label_value_with_comma_inside_quotes() {
        let h = parse_column_header(r#"{path="a,b"}"#).expect("comma in quoted value must parse");
        assert_eq!(h.labels.get("path").map(|s| s.as_str()), Some("a,b"));
    }

    #[test]
    fn multiple_labels_three() {
        let h = parse_column_header(
            r#"{__name__="metric", instance="host:9090", job="prom", env="prod"}"#,
        )
        .expect("multiple labels must parse");
        assert_eq!(h.metric_name.as_deref(), Some("metric"));
        assert_eq!(h.labels.len(), 3);
        assert_eq!(h.labels.get("env").map(|s| s.as_str()), Some("prod"));
    }

    // -----------------------------------------------------------------------
    // parse_column_header: error cases
    // -----------------------------------------------------------------------

    #[test]
    fn unmatched_open_brace_returns_error() {
        let result = parse_column_header("{instance=\"foo\"");
        assert!(result.is_err(), "unmatched brace must error");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("missing closing '}'"), "got: {msg}");
    }

    #[test]
    fn missing_equals_returns_error() {
        let result = parse_column_header("{instance}");
        assert!(result.is_err(), "missing = must error");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("'='"), "got: {msg}");
    }

    #[test]
    fn empty_key_returns_error() {
        let result = parse_column_header(r#"{="value"}"#);
        assert!(result.is_err(), "empty key must error");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("empty label key"), "got: {msg}");
    }

    #[test]
    fn unterminated_quoted_value_returns_error() {
        let result = parse_column_header(r#"{key="unterminated}"#);
        // The `}` is consumed as part of the quoted string, so it is unterminated.
        assert!(result.is_err(), "unterminated quote must error");
    }

    // -----------------------------------------------------------------------
    // split_csv_header_fields
    // -----------------------------------------------------------------------

    #[test]
    fn split_simple_unquoted_fields() {
        let fields = split_csv_header_fields("timestamp,cpu,mem");
        assert_eq!(fields, vec!["timestamp", "cpu", "mem"]);
    }

    #[test]
    fn split_quoted_fields_strip_outer_quotes() {
        let fields = split_csv_header_fields(r#""Time","Value""#);
        assert_eq!(fields, vec!["Time", "Value"]);
    }

    #[test]
    fn split_rfc4180_escaped_quotes() {
        // CSV: "Time","{__name__=""up"", job=""prom""}"
        let line = r#""Time","{__name__=""up"", job=""prom""}""#;
        let fields = split_csv_header_fields(line);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0], "Time");
        assert_eq!(fields[1], r#"{__name__="up", job="prom"}"#);
    }

    #[test]
    fn split_empty_line() {
        let fields = split_csv_header_fields("");
        assert_eq!(fields, vec![""]);
    }

    #[test]
    fn split_single_field() {
        let fields = split_csv_header_fields("timestamp");
        assert_eq!(fields, vec!["timestamp"]);
    }

    #[test]
    fn split_mixed_quoted_and_unquoted() {
        let fields = split_csv_header_fields(r#"Time,"cpu_percent",mem"#);
        assert_eq!(fields, vec!["Time", "cpu_percent", "mem"]);
    }

    #[test]
    fn split_grafana_style_header() {
        let line = r#""Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}","{__name__=""up"", instance=""localhost:9100"", job=""node""}""#;
        let fields = split_csv_header_fields(line);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], "Time");
        assert_eq!(
            fields[1],
            r#"{__name__="up", instance="localhost:9090", job="prometheus"}"#
        );
        assert_eq!(
            fields[2],
            r#"{__name__="up", instance="localhost:9100", job="node"}"#
        );
    }

    // -----------------------------------------------------------------------
    // parse_header_row
    // -----------------------------------------------------------------------

    #[test]
    fn parse_header_row_plain_columns() {
        let headers = parse_header_row("timestamp,cpu_percent,mem_percent")
            .expect("plain headers must parse");
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].metric_name.as_deref(), Some("timestamp"));
        assert_eq!(headers[1].metric_name.as_deref(), Some("cpu_percent"));
        assert_eq!(headers[2].metric_name.as_deref(), Some("mem_percent"));
    }

    #[test]
    fn parse_header_row_grafana_export() {
        let line = r#""Time","{__name__=""up"", instance=""localhost:9090"", job=""prometheus""}","{__name__=""up"", instance=""localhost:9100"", job=""node""}""#;
        let headers = parse_header_row(line).expect("grafana headers must parse");
        assert_eq!(headers.len(), 3);

        // Column 0: Time
        assert_eq!(headers[0].metric_name.as_deref(), Some("Time"));
        assert!(headers[0].labels.is_empty());

        // Column 1: up with prometheus labels
        assert_eq!(headers[1].metric_name.as_deref(), Some("up"));
        assert_eq!(
            headers[1].labels.get("instance").map(|s| s.as_str()),
            Some("localhost:9090")
        );
        assert_eq!(
            headers[1].labels.get("job").map(|s| s.as_str()),
            Some("prometheus")
        );

        // Column 2: up with node labels
        assert_eq!(headers[2].metric_name.as_deref(), Some("up"));
        assert_eq!(
            headers[2].labels.get("instance").map(|s| s.as_str()),
            Some("localhost:9100")
        );
        assert_eq!(
            headers[2].labels.get("job").map(|s| s.as_str()),
            Some("node")
        );
    }

    #[test]
    fn parse_header_row_format2_mixed() {
        let line = r#"Time,up{instance="host1"},up{instance="host2"}"#;
        let headers = parse_header_row(line).expect("format2 headers must parse");
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[1].metric_name.as_deref(), Some("up"));
        assert_eq!(
            headers[1].labels.get("instance").map(|s| s.as_str()),
            Some("host1")
        );
        assert_eq!(headers[2].metric_name.as_deref(), Some("up"));
        assert_eq!(
            headers[2].labels.get("instance").map(|s| s.as_str()),
            Some("host2")
        );
    }

    // -----------------------------------------------------------------------
    // Unquoted label values
    // -----------------------------------------------------------------------

    #[test]
    fn unquoted_label_value() {
        let h = parse_column_header("{key=value}").expect("unquoted value must parse");
        assert_eq!(h.labels.get("key").map(|s| s.as_str()), Some("value"));
    }

    // -----------------------------------------------------------------------
    // Contract: ParsedColumnHeader is Send + Sync
    // -----------------------------------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn parsed_column_header_is_send_and_sync() {
        assert_send_sync::<ParsedColumnHeader>();
    }

    // -----------------------------------------------------------------------
    // Determinism: parsing the same header twice yields identical results
    // -----------------------------------------------------------------------

    #[test]
    fn determinism_same_header_twice() {
        let header = r#"{__name__="up", instance="localhost:9090", job="prometheus"}"#;
        let a = parse_column_header(header).expect("first parse");
        let b = parse_column_header(header).expect("second parse");
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // is_header_line
    // -----------------------------------------------------------------------

    #[test]
    fn is_header_line_detects_text_header() {
        assert!(is_header_line("timestamp,cpu,mem"));
    }

    #[test]
    fn is_header_line_rejects_all_numeric() {
        assert!(!is_header_line("1000,42.5,99.1"));
    }

    #[test]
    fn is_header_line_single_column_text() {
        assert!(is_header_line("metric_name"));
    }

    #[test]
    fn is_header_line_single_column_numeric() {
        assert!(!is_header_line("42.5"));
    }

    #[test]
    fn is_header_line_first_col_numeric_second_text() {
        assert!(is_header_line("1000,cpu_percent"));
    }

    #[test]
    fn is_header_line_empty_string_is_non_numeric() {
        // An empty string cannot be parsed as f64, so it is classified as a header.
        assert!(is_header_line(""));
    }
}
