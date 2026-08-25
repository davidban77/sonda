//! Write normalized series as a CSV `csv_replay` already knows how to read.
//!
//! Pure and feature-free. This module invents nothing: the header grammar it
//! emits is the one [`crate::generator::csv_header`] parses, and the tests
//! round-trip through that parser rather than through a transcription of it.
//!
//! # The two escaping layers, in order
//!
//! A column header is a label block nested inside a CSV field, so a label
//! value passes through two encoders on the way out and two decoders on the
//! way back:
//!
//! 1. **Label block.** `{__name__="m", job="api"}`. Inside a quoted value a
//!    backslash escapes the next character, so a literal `"` is written `\"`
//!    and a literal `\` is written `\\`.
//! 2. **CSV field.** The whole block is then wrapped in double quotes with
//!    every `"` doubled, per RFC 4180. Headers are quoted unconditionally —
//!    they always contain `"` and usually `,`, so there is no case where
//!    leaving them bare would be correct.
//!
//! # Newlines are refused, not mangled
//!
//! The label grammar's escape is "backslash means take the next character
//! literally", which can express a quote or a backslash but has no way to
//! express a newline: `\n` decodes to the letter `n`. A CSV is also read line
//! by line, so a literal newline in a header would split one column across two
//! rows. Rather than silently corrupting the capture, a label value containing
//! `\n` or `\r` is rejected with the offending label named.

use super::normalize::{Grid, NormalizedSeries};
use crate::{ConfigError, SondaError};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Build one column header for a series' label set.
///
/// Emits format 1 — `{__name__="m", k="v"}` — when the series kept its metric
/// name, and format 3 — `{k="v"}` — when the query dropped it (an aggregation
/// like `sum by (job)`). In the latter case the caller must set
/// `default_metric_name` on the generator config, because
/// `auto_discover_specs` refuses a nameless column without one.
///
/// The returned string is the *unquoted* header; [`csv_quote_field`] applies
/// the CSV layer.
///
/// # Errors
///
/// Returns [`SondaError::Config`] when a label key or value contains a
/// newline, or when a key contains `=`. Neither is representable in this
/// grammar — see below for why `=` is the only delimiter that needs refusing.
pub fn column_header(labels: &BTreeMap<String, String>) -> Result<String, SondaError> {
    for (k, v) in labels {
        for (what, s) in [("key", k), ("value", v)] {
            if s.contains('\n') || s.contains('\r') {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "csv capture: label {what} {s:?} (of label {k:?}) contains a newline, \
                     which a CSV column header cannot represent. Drop or rewrite the label \
                     with a PromQL `label_replace` before capturing."
                ))));
            }
        }

        // Values are escaped on the way out; keys are not, because the `{k="v"}`
        // grammar has nowhere to put an escape on the left of the `=`. So a key
        // containing the delimiter writes a header the parser reads back as a
        // different label: key `a=b` with value `v` emits `a=b="v"`, and
        // `parse_column_header` splits on the first `=` and returns a label
        // named `a` whose value is `b="v"`. Silent, and wrong data in the
        // captured file.
        //
        // Only `=` needs this. Measured against the real parser, keys containing
        // `"`, `{`, `}`, a space or a comma all round-trip unharmed, so refusing
        // them "for symmetry" would reject captures that work today. The tests
        // below pin that, so the narrowness of this guard is checked rather than
        // asserted.
        //
        // Refused rather than escaped: a conforming Prometheus label name is
        // `[a-zA-Z_][a-zA-Z0-9_]*`, so this cannot arrive from a well-behaved
        // server — but acquisition reads a remote TSDB, which is the trust
        // boundary this whole module polices, and failing at capture time is
        // more honest than writing a header that reads back as something else.
        if k.contains('=') {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "csv capture: label key {k:?} contains '=', which is the delimiter this \
                 column-header grammar uses to separate a label key from its value. A key \
                 containing it would be read back as a different label. Drop or rewrite the \
                 label with a PromQL `label_replace` before capturing."
            ))));
        }
    }

    let mut out = String::from("{");
    let mut first = true;
    // __name__ leads so the header reads like the series does. The rest follow
    // in BTreeMap order, which makes the emitted file deterministic.
    if let Some(name) = labels.get("__name__") {
        let _ = write!(out, "__name__=\"{}\"", escape_label_value(name));
        first = false;
    }
    for (k, v) in labels.iter().filter(|(k, _)| k.as_str() != "__name__") {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let _ = write!(out, "{}=\"{}\"", k, escape_label_value(v));
    }
    out.push('}');
    Ok(out)
}

/// Escape a label value for the inside of a `"..."` label block value.
fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch == '\\' || ch == '"' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Wrap a field in RFC 4180 quotes, doubling any embedded quote.
pub fn csv_quote_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for ch in field.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Render the whole capture as CSV text.
///
/// Column 0 is the timestamp in unix seconds — `csv_replay` derives its replay
/// rate from the delta between the first two, and `auto_discover_specs` skips
/// column 0 when mapping columns to series. Every other column is one series.
///
/// # Errors
///
/// Propagates [`column_header`]'s newline rejection.
pub fn write_csv(grid: Grid, series: &[NormalizedSeries]) -> Result<String, SondaError> {
    let mut out = String::new();

    out.push_str("timestamp");
    for s in series {
        out.push(',');
        out.push_str(&csv_quote_field(&column_header(&s.labels)?));
    }
    out.push('\n');

    for n in 0..grid.len {
        // {:.3} rather than {} so a timestamp never reaches the file in
        // scientific notation, which parse_csv_timestamp would reject.
        let _ = write!(out, "{:.3}", grid.point(n));
        for s in series {
            out.push(',');
            match s.values.get(n) {
                // Display for f64 round-trips through parse. No formatting
                // decision is made about the value itself.
                //
                // What a literal "NaN" MEANS on the way back in is the
                // opposite of what this comment used to claim: the reader
                // treats it as a sample that is present and happens to be NaN.
                // Only a BLANK cell is absence, and only a blank is
                // cross-checked against a declared `gap_windows:` entry
                // (`csv_replay::column_values_and_gaps`). Emitting "NaN" here therefore
                // reproduces the value and the timing but NOT the silence.
                // Wiring this side to emit blanks plus the matching windows is
                // WP18b's job; until then this is the honest half.
                Some(v) => {
                    let _ = write!(out, "{v}");
                }
                None => out.push_str("NaN"),
            }
        }
        out.push('\n');
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::csv_header::parse_header_row;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Round-trip one label set through the emitter and the *real* parser.
    ///
    /// Drives `parse_header_row`, not a copy of its rules — a transcription
    /// would diverge on exactly the line a bug is on.
    fn roundtrip(pairs: &[(&str, &str)]) -> (Option<String>, BTreeMap<String, String>) {
        let l = labels(pairs);
        let header = column_header(&l).expect("header must build");
        let line = format!("timestamp,{}", csv_quote_field(&header));
        let parsed = parse_header_row(&line).expect("the real parser must accept our header");
        assert_eq!(parsed.len(), 2, "timestamp column plus one series column");
        let col = &parsed[1];
        let got: BTreeMap<String, String> = col
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        (col.metric_name.clone(), got)
    }

    #[test]
    fn plain_labels_round_trip_through_the_real_parser() {
        let (name, got) = roundtrip(&[("__name__", "http_requests_total"), ("job", "api")]);
        assert_eq!(name.as_deref(), Some("http_requests_total"));
        assert_eq!(got, labels(&[("job", "api")]));
    }

    #[test]
    fn a_query_that_dropped_the_metric_name_emits_a_nameless_header() {
        let (name, got) = roundtrip(&[("job", "api")]);
        assert_eq!(name, None, "format 3 — caller must set default_metric_name");
        assert_eq!(got, labels(&[("job", "api")]));
    }

    // ---- Law 4: hostile input, one row per shape, each round-tripped ----
    //
    // The value table below was the whole of this coverage for a while, and
    // that was the gap: `{key="value"}` has three positions and only one of
    // them was ever varied. The key and name tables came after a review
    // pointed out that eighteen shapes of one dimension is still one
    // dimension — and the key path had a real hole.

    /// Keys are NOT escaped on the way out, so they get their own table.
    ///
    /// `=` is refused because it is the delimiter (see `column_header`).
    /// Everything else here round-trips, and that is asserted rather than
    /// assumed: the guard is deliberately narrow, and a future "harden the
    /// keys" change that started refusing these would fail this test rather
    /// than silently reject captures that work.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::quote(          "a\"b")]
    #[case::backslash(      "a\\b")]
    #[case::close_brace(    "a}b")]
    #[case::open_brace(     "a{b")]
    #[case::space(          "a b")]
    #[case::comma(          "a,b")]
    #[case::colon(          "a:b")]
    #[case::unicode(        "café_ünïcode")]
    #[case::leading_digit(  "0abc")]
    #[case::plain(          "job")]
    fn hostile_label_keys_survive_the_round_trip(#[case] key: &str) {
        let (name, got) = roundtrip(&[("__name__", "m"), (key, "v")]);
        assert_eq!(name.as_deref(), Some("m"));
        assert_eq!(
            got,
            labels(&[(key, "v")]),
            "key {key:?} must come back as the same key"
        );
    }

    /// A key containing the delimiter is refused at capture, not mangled.
    ///
    /// Before this guard, key `a=b` with value `v` wrote `a=b="v"`, which the
    /// real parser split on the first `=` and read back as a label named `a`
    /// whose value was `b="v"` — no error, wrong data in the captured file.
    #[test]
    fn a_label_key_containing_the_delimiter_is_refused_rather_than_mangled() {
        let err = column_header(&labels(&[("__name__", "m"), ("a=b", "v")]))
            .expect_err("a key containing '=' must be refused");
        let msg = err.to_string();
        assert!(msg.contains("a=b"), "error names the key: {msg}");
        assert!(msg.contains('='), "error names the delimiter: {msg}");
    }

    /// An empty key is refused by the parser rather than accepted silently —
    /// pinned here so the capture path keeps failing loudly on it.
    #[test]
    fn an_empty_label_key_is_refused_somewhere_in_the_path() {
        let header = column_header(&labels(&[("__name__", "m"), ("", "v")]))
            .expect("column_header does not police empty keys");
        let line = format!("timestamp,{}", csv_quote_field(&header));
        let err = parse_header_row(&line).expect_err("the parser must refuse an empty key");
        assert!(
            err.to_string().contains("empty label key"),
            "parser names the fault: {err}"
        );
    }

    /// The metric name is the third position, and it was never varied either.
    ///
    /// It rides in `__name__`, which `column_header` writes first and without
    /// escaping the key — so these exercise the value-escaping layer on a
    /// label the parser treats specially.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::quote(       "m\"x")]
    #[case::brace(       "m}x")]
    #[case::comma(       "m,x")]
    #[case::equals(      "m=x")]
    #[case::space(       "m x")]
    #[case::unicode(     "café_metric")]
    #[case::plain(       "http_requests_total")]
    fn hostile_metric_names_survive_the_round_trip(#[case] name: &str) {
        let (got_name, got) = roundtrip(&[("__name__", name), ("job", "api")]);
        assert_eq!(got_name.as_deref(), Some(name), "name {name:?} must survive");
        assert_eq!(got, labels(&[("job", "api")]));
    }

    // ---- Law 4: hostile VALUES — the original dimension ----

    #[test]
    fn hostile_label_values_survive_both_escaping_layers() {
        let cases: &[(&str, &str)] = &[
            ("comma", "a,b"),
            ("double_quote", "a\"b"),
            ("backslash", "a\\b"),
            ("backslash_then_quote", "a\\\"b"),
            ("close_brace", "a}b"),
            ("trailing_close_brace", "a}"),
            ("open_brace", "a{b"),
            ("equals", "a=b"),
            ("colon_space", "a: b"),
            ("quoted_number", "200"),
            ("empty", ""),
            ("spaces", "  padded  "),
            ("unicode", "café—ünïcode"),
            ("emoji", "🔥"),
            ("csv_injection", "=cmd|'/c calc'!A1"),
            ("yaml_ish", "*anchor &ref"),
            ("looks_like_header", "{__name__=\"other\"}"),
            ("tab", "a\tb"),
        ];
        for (case, value) in cases {
            let (name, got) = roundtrip(&[("__name__", "m"), ("job", value)]);
            assert_eq!(name.as_deref(), Some("m"), "case {case}: name survived");
            assert_eq!(
                got.get("job").map(String::as_str),
                Some(*value),
                "case {case}: value must survive both layers verbatim"
            );
        }
    }

    #[test]
    fn a_value_that_is_itself_a_label_block_does_not_escape_its_field() {
        // The nastiest shape: a label value that looks like the syntax around
        // it. If either layer under-escaped, the parser would read extra
        // labels or lose the column.
        let (name, got) = roundtrip(&[
            ("__name__", "m"),
            ("job", "\"}, injected=\"yes"),
            ("real", "kept"),
        ]);
        assert_eq!(name.as_deref(), Some("m"));
        assert_eq!(got.len(), 2, "exactly the two labels emitted, no injection");
        assert_eq!(
            got.get("job").map(String::as_str),
            Some("\"}, injected=\"yes")
        );
        assert_eq!(got.get("real").map(String::as_str), Some("kept"));
    }

    #[test]
    fn newline_in_a_label_value_is_refused_with_the_label_named() {
        let err = column_header(&labels(&[("__name__", "m"), ("job", "a\nb")]))
            .expect_err("a newline must be refused");
        let msg = err.to_string();
        assert!(msg.contains("newline"), "message says what is wrong: {msg}");
        assert!(msg.contains("job"), "message names the label: {msg}");
    }

    #[test]
    fn carriage_return_is_refused_too() {
        assert!(column_header(&labels(&[("job", "a\rb")])).is_err());
    }

    // ---- the file as a whole ----

    fn norm(pairs: &[(&str, &str)], values: &[f64]) -> NormalizedSeries {
        NormalizedSeries {
            labels: labels(pairs),
            values: values.to_vec(),
        }
    }

    #[test]
    fn the_emitted_file_is_detected_as_having_a_header() {
        let g = Grid::new(1000.0, 1020.0, 10.0).expect("grid");
        let csv = write_csv(
            g,
            &[norm(&[("__name__", "m"), ("job", "api")], &[1.0, 2.0, 3.0])],
        )
        .expect("csv");
        let first = csv.lines().next().expect("a header line");
        assert!(
            crate::generator::csv_header::is_header_line(first),
            "the engine must recognise our header row: {first}"
        );
    }

    #[test]
    fn timestamps_are_the_grid_and_values_line_up_with_them() {
        let g = Grid::new(1000.0, 1020.0, 10.0).expect("grid");
        let csv = write_csv(g, &[norm(&[("__name__", "m")], &[1.0, 2.0, 3.0])]).expect("csv");
        let rows: Vec<&str> = csv.lines().skip(1).collect();
        assert_eq!(rows, vec!["1000.000,1", "1010.000,2", "1020.000,3"]);
    }

    #[test]
    fn a_gap_is_written_as_nan_so_the_row_count_matches_the_grid() {
        // The property the whole gap decision rests on: one row per grid
        // point, whether or not there was data.
        let g = Grid::new(0.0, 30.0, 10.0).expect("grid");
        let csv =
            write_csv(g, &[norm(&[("__name__", "m")], &[1.0, f64::NAN, 3.0, 4.0])]).expect("csv");
        let rows: Vec<&str> = csv.lines().skip(1).collect();
        assert_eq!(rows.len(), 4, "one row per grid point, gap included");
        assert_eq!(rows[1], "10.000,NaN");
    }

    #[test]
    fn several_series_become_several_columns_in_a_stable_order() {
        let g = Grid::new(0.0, 10.0, 10.0).expect("grid");
        let csv = write_csv(
            g,
            &[
                norm(&[("__name__", "m"), ("i", "a")], &[1.0, 2.0]),
                norm(&[("__name__", "m"), ("i", "b")], &[10.0, 20.0]),
            ],
        )
        .expect("csv");
        let parsed = parse_header_row(csv.lines().next().expect("header")).expect("parses");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].labels.get("i").map(String::as_str), Some("a"));
        assert_eq!(parsed[2].labels.get("i").map(String::as_str), Some("b"));
        let rows: Vec<&str> = csv.lines().skip(1).collect();
        assert_eq!(rows, vec!["0.000,1,10", "10.000,2,20"]);
    }

    #[test]
    fn values_round_trip_through_f64_parse_without_precision_loss() {
        let g = Grid::new(0.0, 30.0, 10.0).expect("grid");
        let awkward = [0.1 + 0.2, 1e-9, 1.7976931348623157e308, -0.0];
        let csv = write_csv(g, &[norm(&[("__name__", "m")], &awkward)]).expect("csv");
        for (row, expected) in csv.lines().skip(1).zip(awkward.iter()) {
            let cell = row.split(',').nth(1).expect("value cell");
            let got: f64 = cell.parse().expect("the engine parses with f64::from_str");
            assert_eq!(
                got.to_bits(),
                expected.to_bits(),
                "value {expected} must survive verbatim, got {cell}"
            );
        }
    }
}
