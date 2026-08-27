//! Write normalized series as a CSV `csv_replay` already knows how to read.
//!
//! Pure and feature-free. The header grammar emitted here is the one
//! [`crate::generator::csv_header`] parses, and the tests round-trip through
//! that parser rather than a transcription of it.
//!
//! A column header is a label block nested inside a CSV field, so it passes
//! through two encoders: the label block backslash-escapes `"` and `\\`, then
//! the whole thing is RFC 4180 quoted with `"` doubled. Headers are quoted
//! unconditionally — they always contain `"` and usually `,`.
//!
//! Newlines are refused rather than mangled: the label grammar's escape cannot
//! express one (`\\n` decodes to the letter `n`), and a CSV is read line by
//! line, so a literal newline would split one column across two rows.

use super::normalize::{Grid, NormalizedSeries};
use crate::config::GapWindowConfig;
use crate::generator::csv_replay::tick_in_window;
use crate::{ConfigError, SondaError};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The `gap_windows:` entries describing one column's absent grid points — one
/// window per maximal run, in row order.
///
/// Blanks and windows are one artifact: `cross_check_gap_windows` refuses a
/// capture where they disagree in either direction.
///
/// Both edges sit on the **midpoints** either side of the run — `at = (a-0.5)*step`
/// clamped to zero at row 0, `end = (b+0.5)*step` — not on the row instants
/// themselves. Containment is `t*step >= at && t*step < end`, and `at`/`for`
/// reach that predicate via a decimal string and microsecond truncation while
/// the predicate recomputes `t*step` directly. Landing an edge on a row instant
/// puts it a rounding error from the boundary, in either direction; half a step
/// of margin does not.
///
/// Then it checks its own work: re-parses what it built and asks
/// [`tick_in_window`] — the predicate the cross-check uses — whether it covers
/// exactly the intended rows.
///
/// # Errors
///
/// [`SondaError::Config`] if the window fails that round-trip, which now needs a
/// step whose half-width is under a microsecond.
pub fn gap_windows_for(
    values: &[Option<f64>],
    step_secs: f64,
) -> Result<Vec<GapWindowConfig>, SondaError> {
    let mut windows = Vec::new();
    let mut row = 0usize;

    while row < values.len() {
        if values[row].is_some() {
            row += 1;
            continue;
        }
        let first = row;
        while row < values.len() && values[row].is_none() {
            row += 1;
        }
        let last = row - 1;

        // Clamped at zero: a run starting on row 0 has no earlier row to
        // exclude, and `at` is an offset from scenario start so it cannot go
        // negative.
        let at_secs = if first == 0 {
            0.0
        } else {
            (first as f64 - 0.5) * step_secs
        };
        let end_secs = (last as f64 + 0.5) * step_secs;
        let window = GapWindowConfig {
            at: format!("{at_secs}s"),
            r#for: format!("{}s", end_secs - at_secs),
        };
        verify_window(&window, first, last, values.len(), step_secs)?;
        windows.push(window);
    }

    Ok(windows)
}

/// Confirm an emitted window covers rows `first..=last` and nothing else.
///
/// Only the boundary rows can be wrong — containment is monotone in row index —
/// so this checks the run's two ends and its two neighbours, via the real parser
/// and the real predicate.
fn verify_window(
    window: &GapWindowConfig,
    first: usize,
    last: usize,
    row_count: usize,
    step_secs: f64,
) -> Result<(), SondaError> {
    let (at, len) = window.resolve()?;
    let pair = (at.as_secs_f64(), at.as_secs_f64() + len.as_secs_f64());

    let complain = |row: usize, expected: bool| {
        SondaError::Config(ConfigError::invalid(format!(
            "acquire: emitted gap window {{at: {}, for: {}}} for absent rows {first}..={last} \
             {} row {row} after round-tripping through duration parsing at a {step_secs}s step. \
             Durations resolve to whole microseconds, and this window's edges sit half a step \
             from the rows either side, so a step below about 2us cannot be expressed. Capture \
             at a coarser step.",
            window.at,
            window.r#for,
            if expected { "does not cover" } else { "covers" },
        )))
    };

    for row in [first, last] {
        if !tick_in_window(row, step_secs, pair) {
            return Err(complain(row, true));
        }
    }
    if first > 0 && tick_in_window(first - 1, step_secs, pair) {
        return Err(complain(first - 1, false));
    }
    if last + 1 < row_count && tick_in_window(last + 1, step_secs, pair) {
        return Err(complain(last + 1, false));
    }
    Ok(())
}

/// Build one column header for a series' label set, unquoted —
/// [`csv_quote_field`] applies the CSV layer.
///
/// `{__name__="m", k="v"}` when the series kept its metric name, `{k="v"}` when
/// the query dropped it. In the latter case the caller must set
/// `default_metric_name`, which `auto_discover_specs` requires for a nameless
/// column.
///
/// # Errors
///
/// [`SondaError::Config`] when a key or value contains a newline, or a key
/// contains `=`; neither is representable in this grammar.
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

        // Values are escaped on the way out, keys are not — the `{k="v"}`
        // grammar has nowhere to put an escape left of the `=`. A key containing
        // the delimiter therefore reads back as a different label: `a=b` with
        // value `v` emits `a=b="v"`, which the parser splits into label `a` with
        // value `b="v"`. Silent, and wrong data in the file.
        //
        // Only `=` needs refusing; against the real parser, keys containing `"`,
        // `{`, `}`, space or comma all round-trip unharmed, so refusing
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

/// Render one grid instant as the timestamp cell for its row.
///
/// `{:.3}` rather than `{}` keeps scientific notation out of the file, which
/// `parse_csv_timestamp` rejects.
///
/// A function rather than an inline format string because this precision decides
/// the step the engine replays at — `csv_replay` derives its interval from these
/// timestamps — so [`super::yaml_out`] calls it to reduce the same numbers the
/// file will carry.
pub fn format_timestamp(instant: f64) -> String {
    format!("{instant:.3}")
}

/// Render the whole capture as CSV text.
///
/// Column 0 is the timestamp in unix seconds — `csv_replay` derives its replay
/// rate from the delta between the first two, and `auto_discover_specs` skips
/// column 0 when mapping columns to series. Every other column is one series.
///
/// This writes every series' blanks, while [`gap_windows_for`] speaks for one
/// column. `gap_windows:` is scenario-level, so a caller writing a multi-column
/// capture must group columns by identical absence pattern and emit one block
/// per group — see [`super::yaml_out`]. Nothing here can check that, because
/// nothing here knows which column the windows will be paired with.
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
        out.push_str(&format_timestamp(grid.point(n)));
        for s in series {
            out.push(',');
            // A blank is the only thing that reads back as absence — a literal
            // "NaN" reads as a present sample that happens to be NaN. So a
            // reported value is written verbatim (NaN and infinities included,
            // since those are data), and both flavours of absence write nothing:
            // `Some(None)` is a grid point with no sample, `None` a series
            // shorter than the grid.
            if let Some(Some(v)) = s.values.get(n) {
                let _ = write!(out, "{v}");
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

    /// Every entry present. Use [`norm_opt`] to express absence.
    fn norm(pairs: &[(&str, &str)], values: &[f64]) -> NormalizedSeries {
        norm_opt(pairs, &values.iter().map(|v| Some(*v)).collect::<Vec<_>>())
    }

    fn norm_opt(pairs: &[(&str, &str)], values: &[Option<f64>]) -> NormalizedSeries {
        NormalizedSeries {
            labels: labels(pairs),
            values: values.to_vec(),
        }
    }

    /// `None` for absent, `Some` for present — including a present NaN.
    fn absent_at(len: usize, absent: &[usize]) -> Vec<Option<f64>> {
        (0..len)
            .map(|i| {
                if absent.contains(&i) {
                    None
                } else {
                    Some(i as f64 + 1.0)
                }
            })
            .collect()
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
    fn a_gap_is_written_as_a_blank_and_still_holds_its_row() {
        // Two properties at once. One row per grid point whether or not there
        // was data — the alignment the whole gap decision rests on — and the
        // absent cell written BLANK rather than "NaN", because only a blank
        // reads back as absence.
        let g = Grid::new(0.0, 30.0, 10.0).expect("grid");
        let csv = write_csv(
            g,
            &[norm_opt(
                &[("__name__", "m")],
                &[Some(1.0), None, Some(3.0), Some(4.0)],
            )],
        )
        .expect("csv");
        let rows: Vec<&str> = csv.lines().skip(1).collect();
        assert_eq!(rows.len(), 4, "one row per grid point, gap included");
        assert_eq!(rows[1], "10.000,");
    }

    #[test]
    fn a_reported_nan_is_a_value_and_is_never_written_as_absence() {
        // `response` keeps a TSDB-reported NaN verbatim on purpose
        // (`non_finite_values_survive_verbatim` pins it), so `Some(NaN)` is a
        // sample and `None` is silence. Writing the former as a blank would
        // declare a `gap_windows:` entry over a point the database reported —
        // the mirror image of the bug blanks were introduced to fix.
        let g = Grid::new(0.0, 20.0, 10.0).expect("grid");
        let values = [Some(1.0), Some(f64::NAN), Some(3.0)];
        let csv = write_csv(g, &[norm_opt(&[("__name__", "m")], &values)]).expect("csv");
        let rows: Vec<&str> = csv.lines().skip(1).collect();
        assert_eq!(rows[1], "10.000,NaN", "a reported NaN stays a value");

        assert!(
            gap_windows_for(&values, 10.0).expect("windows").is_empty(),
            "a reported NaN declares no silence"
        );
        // And the reader agrees it is not a blank.
        let (_, blanks) =
            crate::generator::csv_replay::column_values_and_gaps(&csv, 1).expect("read back");
        assert!(blanks.is_empty(), "no blank rows, so nothing to cover");
    }

    /// The blanks and the windows are one artifact, so they are tested as one.
    ///
    /// Drives the real reader (`column_values_and_gaps`) and the real validator
    /// (`cross_check_gap_windows`) over a file this module wrote. A window that
    /// is off by one row — or one float — fails here rather than at a user's
    /// first run.
    fn round_trip(values: &[Option<f64>], step: f64) {
        let grid = Grid::new(0.0, step * (values.len() - 1) as f64, step).expect("grid");
        let csv = write_csv(grid, &[norm_opt(&[("__name__", "m")], values)]).expect("csv");
        let windows = gap_windows_for(values, step).expect("windows");

        // Column 1: index 0 is the `timestamp` column every emitted file leads
        // with, so reading 0 here would compare against the grid instead of the
        // series and find no blanks at all.
        let (read_back, blanks) =
            crate::generator::csv_replay::column_values_and_gaps(&csv, 1).expect("read back");

        let expected_blanks: Vec<usize> = values
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_none())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(blanks, expected_blanks, "blank rows survive the write/read");
        assert_eq!(read_back.len(), values.len(), "grid stays aligned");
        for (i, (got, want)) in read_back.iter().zip(values).enumerate() {
            match want {
                None => assert!(got.is_nan(), "row {i} should read back absent"),
                Some(w) => assert_eq!(got, w, "row {i} value"),
            }
        }

        let pairs: Vec<(f64, f64)> = windows
            .iter()
            .map(|w| {
                let (at, len) = w.resolve().expect("resolve");
                (at.as_secs_f64(), at.as_secs_f64() + len.as_secs_f64())
            })
            .collect();
        let playback = crate::generator::csv_replay::Playback {
            row_count: values.len(),
            repeat: false,
            last_tick: Some(values.len() as u64 - 1),
            bursts: false,
        };
        crate::generator::csv_replay::cross_check_gap_windows(&blanks, &playback, &pairs, step)
            .unwrap_or_else(|e| {
                panic!("emitted windows {windows:?} rejected for values {values:?} at step {step}: {e}")
            });
    }

    #[test]
    fn a_capture_with_gaps_round_trips_through_the_real_reader_and_cross_check() {
        // Leading, interior, trailing, adjacent runs, fully absent, fully present.
        round_trip(&absent_at(4, &[0]), 1.0);
        round_trip(&absent_at(4, &[1, 2]), 1.0);
        round_trip(&absent_at(4, &[3]), 1.0);
        round_trip(&absent_at(5, &[1, 3]), 1.0);
        round_trip(&absent_at(4, &[0, 1, 2, 3]), 1.0);
        round_trip(&absent_at(4, &[]), 1.0);
    }

    #[test]
    fn the_window_boundary_survives_steps_that_break_the_obvious_encoding() {
        // These eleven (step, run) pairs are the ones where `for = (b+1-a)*step`
        // — the obvious encoding — puts row b+1 inside the window after
        // microsecond truncation. They are named individually because they are
        // rare: a sweep of short runs at common steps passes under BOTH
        // encodings, so a test built from round numbers would not discriminate.
        // Verified by mutation: reverting the emitter to (b+1) fails on these.
        // The leading edge has its own failing set, and it is a different one.
        // With `at = a*step` the run's own FIRST row falls outside its window,
        // because `at` reaches the predicate via a decimal string and
        // microsecond quantisation while the predicate recomputes `a * step`
        // directly — and `trunc(x * 1e6) / 1e6` can land marginally ABOVE `x`
        // when the multiply rounds up onto an integer. Containment at the start
        // is `>=`, so marginally above is enough. All of these are steps within
        // a millionth of one second; none appear in the trailing-edge set.
        const SQUARE_LEADING_EDGE_FAILS_HERE: &[(f64, usize, usize)] = &[
            (0.999_999, 13, 13),
            (0.999_999, 13, 14),
            (0.999_999, 13, 15),
            (1.000_001, 19, 19),
            (1.000_001, 19, 20),
            (1.000_001, 19, 21),
        ];
        for &(step, first, last) in SQUARE_LEADING_EDGE_FAILS_HERE {
            let absent: Vec<usize> = (first..=last).collect();
            round_trip(&absent_at(last + 3, &absent), step);
        }

        const OBVIOUS_ENCODING_FAILS_HERE: &[(f64, usize, usize)] = &[
            (0.3, 4, 12),
            (0.3, 8, 22),
            (0.007, 1, 10),
            (0.007, 3, 7),
            (0.007, 3, 12),
            (0.007, 5, 14),
            (0.007, 6, 10),
            (0.007, 6, 14),
            (0.007, 6, 15),
            (0.007, 11, 20),
            (0.007, 12, 21),
        ];
        for &(step, first, last) in OBVIOUS_ENCODING_FAILS_HERE {
            let absent: Vec<usize> = (first..=last).collect();
            round_trip(&absent_at(last + 3, &absent), step);
        }

        // Plus a broad sweep, so the named cases do not become the only
        // coverage if the emitter changes shape.
        for &step in &[0.1, 0.3, 1.0 / 3.0, 0.05, 0.007, 15.0, 0.123_456_789] {
            for run_start in 1..9 {
                for run_len in 1..12 {
                    let absent: Vec<usize> = (run_start..run_start + run_len).collect();
                    round_trip(&absent_at(24, &absent), step);
                }
            }
        }
    }

    #[test]
    fn a_column_with_no_gaps_declares_no_windows() {
        // Vacuity guard for the pair: a window list that is empty because the
        // emitter did nothing is indistinguishable from one that is empty
        // because there was nothing to declare, unless the values are checked.
        let windows = gap_windows_for(&absent_at(3, &[]), 1.0).expect("windows");
        assert!(windows.is_empty());
        let with_gap = gap_windows_for(&absent_at(3, &[1]), 1.0).expect("windows");
        assert_eq!(with_gap.len(), 1, "and a gap does produce one");
    }

    #[test]
    fn one_window_per_run_not_per_absent_row() {
        let windows = gap_windows_for(&absent_at(7, &[1, 2, 3, 5]), 1.0).expect("windows");
        assert_eq!(windows.len(), 2, "two runs, two windows: {windows:?}");
        // Midpoint leading edge: half a step before the run's first row.
        assert_eq!(windows[0].at, "0.5s");
        assert_eq!(windows[1].at, "4.5s");
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
