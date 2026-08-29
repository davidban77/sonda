//! CSV replay value generator -- replays numeric values from a CSV file.
//!
//! Values are loaded once at construction time. Each call to `value()` returns
//! the value at `tick % values.len()` when repeating, or the last value when
//! the tick exceeds the file length (clamped mode).
//!
//! Header detection is automatic: if any non-time field on the first data line
//! fails to parse as `f64`, the line is treated as a header and skipped.

use std::path::Path;

use super::ValueGenerator;
use crate::{ConfigError, GeneratorError, SondaError};

/// A value generator that replays numeric values from a CSV file.
///
/// Reads a column of numeric values from a CSV file at construction time.
/// When `repeat` is true (default), cycles through the values via
/// `values[tick % len]`. When `repeat` is false, returns the last value for
/// ticks beyond the file length.
///
/// Header rows are auto-detected: the first non-comment, non-empty line is
/// inspected and if any non-time column (index > 0) cannot be parsed as
/// `f64`, the line is treated as a header and excluded from data.
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
/// - The first data line is auto-detected as a header when any non-time
///   field is non-numeric.
///
/// # Examples
///
/// ```no_run
/// use sonda_core::generator::csv_replay::CsvReplayGenerator;
/// use sonda_core::generator::ValueGenerator;
///
/// let gen = CsvReplayGenerator::new("data.csv", 0, true).unwrap();
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
    /// The first non-comment, non-empty line is auto-detected as a header
    /// when any non-time field (column index > 0) cannot be parsed as `f64`.
    /// If all fields parse as numbers, the line is treated as data.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CSV file.
    /// * `column` - Zero-based column index to read.
    /// * `repeat` - Whether to cycle values when ticks exceed the value count.
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Generator`] with [`GeneratorError::FileRead`] if
    /// the file cannot be opened or read.
    ///
    /// Returns [`SondaError::Config`] if no valid numeric values are found in
    /// the specified column.
    pub fn new(path: &str, column: usize, repeat: bool) -> Result<Self, SondaError> {
        let file_path = Path::new(path);
        let content = std::fs::read_to_string(file_path).map_err(|e| {
            SondaError::Generator(GeneratorError::FileRead {
                path: path.to_string(),
                source: e,
            })
        })?;

        let values = Self::parse_values(&content, column)?;

        if values.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "CSV file {:?} contains no valid numeric values in column {}",
                path, column
            ))));
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
    pub fn from_str(content: &str, column: usize, repeat: bool) -> Result<Self, SondaError> {
        let values = Self::parse_values(content, column)?;

        if values.is_empty() {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "CSV content contains no valid numeric values in column {}",
                column
            ))));
        }

        Ok(Self { values, repeat })
    }

    /// Detect whether the first data line is a header row.
    ///
    /// Delegates to the shared [`super::csv_header::is_header_line`] function.
    fn is_header_line(line: &str) -> bool {
        super::csv_header::is_header_line(line)
    }

    /// Parse numeric values from CSV content.
    ///
    /// Thin wrapper over [`Self::parse_values_and_gaps`] for callers that do
    /// not need to know which rows were blank.
    fn parse_values(content: &str, column: usize) -> Result<Vec<f64>, SondaError> {
        Self::parse_values_and_gaps(content, column).map(|(values, _)| values)
    }

    /// Parse one column, returning its values and the rows that were blank.
    ///
    /// Comment lines (`#`) and blank *lines* are skipped; the first data line
    /// is auto-detected as a header and skipped when it contains non-numeric
    /// fields. Everything after that is a data row, and every data row
    /// contributes exactly one slot — which is the whole point.
    ///
    /// # Rows and slots line up, and that is load-bearing
    ///
    /// This function used to silently skip any cell it could not parse, with
    /// the comment "Unparseable values are silently skipped". That shortened
    /// the value vector, so a single junk or blank cell made **every later
    /// sample replay one step early** — a whole timeline quietly shifted by
    /// one bad character, with no diagnostic. Replay is meaningless if the
    /// grid can slide, so the rule is now:
    ///
    /// * **blank cell** → `NaN` placeholder, and the row index is reported as
    ///   a gap. The slot is held so nothing after it moves.
    /// * **non-blank cell that will not parse** → hard error naming the row
    ///   and column. There is no reading of `x` that belongs in a timeline.
    /// * **row too short to have this column** → hard error, same reasoning:
    ///   a ragged row is a truncated file, not a gap someone meant.
    ///
    /// # Blank is not `NaN`
    ///
    /// A cell containing the literal text `NaN` is a *present* sample whose
    /// value is NaN — Prometheus returns those, and they replay as data. A
    /// *blank* cell is an absent sample. They are both `f64::NAN` in the
    /// values vector and cannot be told apart there, which is why the blank
    /// rows are returned separately rather than recovered later with
    /// `is_nan()`. Only blanks require a declared gap window.
    ///
    /// Returns `(values, blank_row_indices)`, where the indices are 0-based
    /// positions into `values` (i.e. data rows, not file lines).
    fn parse_values_and_gaps(
        content: &str,
        column: usize,
    ) -> Result<(Vec<f64>, Vec<usize>), SondaError> {
        let mut values = Vec::new();
        let mut blanks = Vec::new();
        let mut first_data_line = true;

        for (index, line) in content.lines().enumerate() {
            // 1-based, and counted over the raw file so the number in an error
            // message is the line a text editor will jump to.
            let line_no = index + 1;
            let trimmed = line.trim();

            // Skip empty lines.
            if trimmed.is_empty() {
                continue;
            }

            // Skip comment lines.
            if trimmed.starts_with('#') {
                continue;
            }

            // Auto-detect header: skip the first data line if it looks like a header.
            if first_data_line {
                first_data_line = false;
                if Self::is_header_line(trimmed) {
                    continue;
                }
            }

            // Split by comma and extract the target column.
            let fields: Vec<&str> = trimmed.split(',').collect();
            let field = fields.get(column).ok_or_else(|| {
                SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: line {line_no} has {} column(s), so column {column} is missing. \
                     Every data row must carry every column — a short row would otherwise drop a \
                     slot and shift the rest of the timeline earlier.",
                    fields.len()
                )))
            })?;

            let cell = field.trim();
            if cell.is_empty() {
                // Absent sample: hold the slot, record the gap.
                blanks.push(values.len());
                values.push(f64::NAN);
                continue;
            }

            let parsed = cell.parse::<f64>().map_err(|_| {
                SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: line {line_no}, column {column}: {cell:?} is not a number. \
                     Leave the cell empty to mark an absent sample (and declare the matching \
                     `gap_windows:` entry); anything else is a typo that would otherwise be \
                     dropped, shifting every later sample one step earlier."
                )))
            })?;
            values.push(parsed);
        }

        Ok((values, blanks))
    }
}

/// Parse one column of CSV text into values plus the rows that were blank.
///
/// Read one CSV column back as values plus the rows that were blank.
///
/// The entry point to [`CsvReplayGenerator::parse_values_and_gaps`] for callers
/// that need the blank rows without building a generator: the config expansion
/// that cross-checks them against `gap_windows:`, and anything verifying a
/// written capture against what it replays.
///
/// Blank rows come back as indices rather than as `NaN` in the values, because
/// a literal `NaN` cell is a reported sample and the two are indistinguishable
/// once merged.
pub fn column_values_and_gaps(
    content: &str,
    column: usize,
) -> Result<(Vec<f64>, Vec<usize>), SondaError> {
    CsvReplayGenerator::parse_values_and_gaps(content, column)
}

/// Check that blank CSV cells and declared gap windows describe the same silence.
///
/// The CSV and the scenario each carry half a claim about an outage: the file
/// leaves a slot empty, and the YAML declares a `gap_windows:` entry covering
/// it. Neither is derived from the other, so either can be edited into
/// disagreement — and both ways of disagreeing are wrong in a way the user
/// would never see at runtime:
///
/// * a **blank with no window** would replay as a `NaN` sample, which is a
///   present value where production had none;
/// * a **window over present data** would suppress a sample that was really
///   recorded, inventing silence that never happened.
///
/// So the two are compared exactly, in both directions, at load. This is
/// cheap: one pass over the rows.
///
/// Row `n` stands for the instant `n * step_secs` after scenario start;
/// windows are half-open `[start, end)`, matching
/// [`is_in_gap_window`](crate::schedule::is_in_gap_window).
///
/// # The check follows the playback, not just the file
///
/// Comparing the file's rows against the windows is only the whole answer when
/// the scenario plays each row exactly once. It does not, by default. Two ways
/// a run reaches an instant the row list does not describe, both of which
/// leaked a `NaN` past a green check before this was written:
///
/// * **`repeat` loops the column.** Row `n` replays at `(k * len + n) *
///   step_secs` for every cycle `k`, and one-shot windows cover the first pass
///   only. Refused outright — see [`Playback::repeat`].
/// * **`repeat: false` clamps.** Past the end of the data the generator holds
///   the final slot for every remaining tick. If that slot is blank, the
///   silence continues for the rest of the run and needs a window that reaches
///   it.
/// * **`bursts:` compress the grid.** Inside a burst window events emit at
///   `rate * multiplier`, so `step_secs` is not constant across the run and row
///   `n` stops landing at `n * step_secs` at all. Refused outright — see
///   [`Playback::bursts`].
///
/// The clamp check asks only whether a *blank* escapes. A window lying over the
/// clamped tail of a present value is not refused: what it silences is a value
/// the generator is holding, not a sample the capture recorded, so
/// "inventing silence that did not happen" does not apply there.
///
/// # What is *not* in this list, and why
///
/// `phase_offset:` (and the `clock_group` chains that compile down to it)
/// delays the whole scenario before the loop's clock starts, so the tick grid
/// and the windows shift together and nothing moves relative to anything else
/// — measured, not assumed. `start_time:` re-anchors the emitted timestamp
/// only; the loop still decides gaps from `elapsed`. `cardinality_spikes:` and
/// `dynamic_labels:` add labels without touching the interval. `jitter:`
/// perturbs the value, not the schedule — it breaks round-trip *equality*,
/// which is the importer's problem, not this one's.
///
/// # Errors
///
/// Returns [`SondaError::Config`] naming the offending rows, capped so a
/// wholly mismatched file reports a readable summary rather than thousands of
/// indices.
pub(crate) fn cross_check_gap_windows(
    blanks: &[usize],
    playback: &Playback,
    windows: &[(f64, f64)],
    step_secs: f64,
) -> Result<(), SondaError> {
    /// How many row numbers to name before summarising.
    const MAX_NAMED: usize = 8;

    let row_count = playback.row_count;

    // A capture containing silence cannot loop. Every one-shot window sits on
    // the first pass, so the second cycle replays the same blank rows at
    // instants no window covers — validation green, `NaN` on the wire. There is
    // no window list that fixes this, because the run is unbounded in cycles,
    // so the answer is a different setting rather than a different window.
    if playback.repeat && !blanks.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: this capture contains {} blank cell(s) and `repeat` is true. \
             A capture containing silence cannot loop: `gap_windows:` describe one pass, \
             so on the second cycle those rows would replay at instants no window covers \
             and emit as NaN samples. Set `repeat: false`.",
            blanks.len(),
        ))));
    }

    // A burst compresses the tick grid. Inside a burst window the loop emits at
    // `rate * multiplier`, so `step_secs` is not one number across the run and
    // "row n plays at n * step_secs" — the assumption every line below rests on
    // — stops holding for every row after the first burst, not just the ones
    // inside it. Measured: `bursts: {every: 4s, for: 2s, multiplier: 4}` on a
    // 1/s capture played all eight rows inside the first two seconds.
    //
    // Refused rather than accounted for, because the windows would have to be
    // recomputed against a grid the user cannot see, and a capture is a record
    // of what happened at the cadence it happened at. Bursts on a capture with
    // no silence stay legal: the grid still slides, but nothing depends on
    // where a particular row lands.
    if playback.bursts && !blanks.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: this capture contains {} blank cell(s) and the scenario declares \
             `bursts:`. A burst emits at `rate * multiplier` inside its window, which \
             compresses the tick grid, so row n no longer plays at n x step and the \
             `gap_windows:` entries would fall on the wrong rows. Remove `bursts:`, or \
             replay a capture that contains no silence.",
            blanks.len(),
        ))));
    }

    // One definition of containment, shared with the interval walk. Two
    // expressions that agree until a float boundary is exactly how the walk
    // came to skip an uncovered tick.
    let covered =
        |row: usize| -> bool { windows.iter().any(|&w| tick_in_window(row, step_secs, w)) };

    let format_rows = |rows: &[usize]| -> String {
        let shown: Vec<String> = rows
            .iter()
            .take(MAX_NAMED)
            .map(|r| (r + 1).to_string())
            .collect();
        if rows.len() > MAX_NAMED {
            format!("{} … and {} more", shown.join(", "), rows.len() - MAX_NAMED)
        } else {
            shown.join(", ")
        }
    };

    let uncovered: Vec<usize> = blanks.iter().copied().filter(|&r| !covered(r)).collect();
    if !uncovered.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: {} blank cell(s) are not covered by any `gap_windows:` entry \
             (data row(s) {}). A blank cell means the sample was absent, which only \
             reproduces as silence if the scenario declares the window — otherwise it \
             would replay as a NaN sample, which is a present value. Add the window, or \
             put the value back in the cell.",
            uncovered.len(),
            format_rows(&uncovered),
        ))));
    }

    let present_but_silenced: Vec<usize> = (0..row_count)
        .filter(|&r| covered(r) && !blanks.contains(&r))
        .collect();
    if !present_but_silenced.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "csv_replay: {} recorded sample(s) fall inside a `gap_windows:` entry \
             (data row(s) {}). The window would suppress data the capture actually has, \
             inventing silence that did not happen. Narrow the window, or blank the cells \
             it covers.",
            present_but_silenced.len(),
            format_rows(&present_but_silenced),
        ))));
    }

    // Past the end of the data the generator holds the final slot. If that slot
    // is blank, every remaining tick is silence the windows still have to
    // account for — the file has no row to hang those instants on, so the
    // per-row pass above cannot see them.
    let last_row = match row_count.checked_sub(1) {
        Some(r) if blanks.contains(&r) => r,
        _ => return Ok(()),
    };
    match playback.last_tick {
        // Unbounded: the held silence never ends, so no finite window reaches
        // it. Say that, rather than naming a window the user could add.
        None => {
            return Err(SondaError::Config(ConfigError::invalid(format!(
                "csv_replay: the capture's last row (data row {}) is blank and the scenario \
                 has no `duration:`. With `repeat: false` the final slot is held for every \
                 later tick, so that silence would run forever and no `gap_windows:` entry \
                 can cover it. Set a `duration:` the windows reach, or put a value in the \
                 last row.",
                last_row + 1,
            ))));
        }
        Some(last_tick) => {
            // Asked as an interval question, not by visiting every tick. The
            // held tail is `duration * rate` long and config drives both:
            // measured, a 24h run at a timescale-multiplied replay rate reaches
            // 86.4M ticks, and enumerating them took 8 seconds and allocated a
            // ~700 MB Vec before reporting a config that was invalid anyway.
            // The windows are hand-written and few, so walking those is
            // bounded by something a human typed.
            if let Some(first) = first_uncovered_tick(last_row + 1, last_tick, windows, step_secs) {
                // The walk computes tick indices from window edges; `covered`
                // is the rule every other branch here uses. Checking the answer
                // against it keeps one definition rather than two that agree
                // until a boundary.
                debug_assert!(
                    !covered(first),
                    "first_uncovered_tick returned tick {first}, which `covered` says is covered"
                );
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "csv_replay: the capture's last row (data row {}) is blank and the \
                     scenario outlives its data. With `repeat: false` that slot is held for \
                     every later tick, and the first such instant is tick {}, which falls \
                     outside every `gap_windows:` entry, where the silence would emit as a \
                     NaN sample. \
                     Extend the window to the end of the run, shorten `duration:`, or put a \
                     value in the last row.",
                    last_row + 1,
                    first,
                ))));
            }
        }
    }

    Ok(())
}

/// How a scenario will walk the rows of its capture.
///
/// The cross-check needs this because the row list alone does not say which
/// instants get played: `repeat` loops it and `repeat: false` clamps past its
/// end. Both reach instants no row describes.
pub(crate) struct Playback {
    /// Rows in the column, blanks included — blanks hold their slot.
    pub row_count: usize,
    /// Resolved `repeat`, after the `unwrap_or(true)` default is applied.
    ///
    /// The default matters: it is the reason a hand-written capture with blanks
    /// looped silently before this check existed.
    pub repeat: bool,
    /// Index of the last tick the scenario plays, or `None` when it has no
    /// `duration:` and runs unbounded.
    pub last_tick: Option<u64>,
    /// Whether the scenario declares `bursts:`.
    ///
    /// A burst changes the emission interval part-way through the run, so there
    /// is no single `step_secs` for a row index to be multiplied by. It is a
    /// flag rather than the window itself because the check refuses the
    /// combination outright — it never needs to know where the burst falls.
    pub bursts: bool,
}

/// Whether tick `t` falls inside one window.
///
/// The single definition of containment in the crate. `covered` is this over
/// every window, and the interval walk uses it as its only oracle — deriving a
/// tick index from a window edge by arithmetic is what let the walk and the
/// predicate disagree (see [`first_uncovered_tick`]).
///
/// `pub(crate)` so the capture side can be judged by the same predicate that
/// judges it on the way back in. `acquire::csv_out` builds windows from runs of
/// absent grid points and then asks *this* whether what it built covers the
/// rows it meant to cover. A second expression of containment over there would
/// diverge from this one at exactly the boundary that matters — which is the
/// failure this comment already records once.
pub(crate) fn tick_in_window(t: usize, step_secs: f64, window: (f64, f64)) -> bool {
    let at = t as f64 * step_secs;
    at >= window.0 && at < window.1
}

/// The first tick in `[from, to]` whose instant no window covers, or `None`
/// when the whole range is covered.
///
/// Walks the windows rather than the ticks. The tick range is `duration * rate`
/// and both come from config, so it is unbounded in practice; the window list
/// is written by hand. Cost is `O(w log n)` — one binary search per window —
/// and enumerating the range is what this exists to avoid.
///
/// # The jump is measured, not computed
///
/// This originally advanced past a window with `ceil(end / step_secs)`, which
/// is float arithmetic reasoning about float arithmetic, and the two disagree
/// at representable boundaries. `at: 100ms, for: 200ms` on a 10 Hz capture —
/// an ordinary thing to write — builds `end = 0.1 + 0.2 = 0.30000000000000004`,
/// and `ceil(end / 0.1)` is `4`, while tick `3` sits exactly *on* `end` and is
/// therefore **not** covered. The walk stepped over an uncovered tick and
/// returned `None`, so the clamp rule accepted a config whose held silence
/// emits `NaN` — the failure the rule exists to refuse, with validation green.
///
/// So there is no arithmetic shortcut here at all. Containment in one window is
/// monotone once inside it — `t * step_secs` only increases, so the covered
/// ticks form one contiguous run — and binary search finds where that run ends
/// by asking [`tick_in_window`] and nothing else.
fn first_uncovered_tick(
    from: usize,
    to: u64,
    windows: &[(f64, f64)],
    step_secs: f64,
) -> Option<usize> {
    // Saturate rather than truncate: `usize` is 32 bits on wasm32, where
    // `as usize` would wrap a large tick count down to a small one and make the
    // walk report an uncovered tick that is past the end of the run.
    let to = usize::try_from(to).unwrap_or(usize::MAX);
    if from > to {
        return None;
    }

    // Sorted by start, so reaching a window that begins after the cursor proves
    // no later window covers the cursor either.
    let mut sorted: Vec<(f64, f64)> = windows.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut cursor = from;
    for window in sorted {
        if (cursor as f64 * step_secs) < window.0 {
            // Before this window, and past every earlier one.
            return Some(cursor);
        }
        if !tick_in_window(cursor, step_secs, window) {
            continue;
        }
        // Inside. If the window also contains the far end, everything in range
        // is covered by it.
        if tick_in_window(to, step_secs, window) {
            return None;
        }
        // Binary search for the boundary: `lo` is always covered by this
        // window, `hi` never is, and they close on the first tick past it.
        let (mut lo, mut hi) = (cursor, to);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if tick_in_window(mid, step_secs, window) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        cursor = hi;
    }
    if cursor <= to {
        Some(cursor)
    } else {
        None
    }
}

impl ValueGenerator for CsvReplayGenerator {
    /// Return the value for the given tick.
    ///
    /// When `repeat` is true, wraps via `tick % len`. When false, clamps to
    /// the last value for ticks beyond the value count.
    fn value(&self, tick: u64) -> f64 {
        let len = self.values.len();
        // Perform modulo in u64 space to avoid truncation on 32-bit platforms
        // where `usize` is 32 bits and ticks above u32::MAX would wrap silently.
        let index = if self.repeat {
            (tick % len as u64) as usize
        } else {
            (tick.min((len - 1) as u64)) as usize
        };
        self.values[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ---- Helper: write content to a temp file and return its path string ------

    fn temp_csv(content: &str) -> (NamedTempFile, String) {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        write!(tmp, "{}", content).expect("write content");
        tmp.flush().expect("flush");
        let path = tmp.path().to_string_lossy().into_owned();
        (tmp, path)
    }

    // ---- Load values from a simple one-column file ----------------------------

    #[test]
    fn one_column_file_loads_all_values() {
        // All-numeric single column: no header auto-detected.
        let content = "1.0\n2.0\n3.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, true).expect("one-column file should load");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    #[test]
    fn one_column_file_from_disk() {
        let (_tmp, path) = temp_csv("10.5\n20.5\n30.5\n");
        let gen =
            CsvReplayGenerator::new(&path, 0, true).expect("one-column disk file should load");
        assert_eq!(gen.value(0), 10.5);
        assert_eq!(gen.value(1), 20.5);
        assert_eq!(gen.value(2), 30.5);
    }

    // ---- Load values from a multi-column CSV with column index ----------------

    #[test]
    fn multi_column_csv_reads_correct_column() {
        // "ts,cpu,mem" is non-numeric in columns 1+ → auto-detected as header.
        let content = "ts,cpu,mem\n1000,42.5,60.0\n2000,55.3,70.1\n3000,18.9,45.2\n";
        let gen = CsvReplayGenerator::from_str(content, 1, true)
            .expect("multi-column should load column 1");
        assert_eq!(gen.value(0), 42.5);
        assert_eq!(gen.value(1), 55.3);
        assert_eq!(gen.value(2), 18.9);
    }

    #[test]
    fn multi_column_csv_reads_first_column() {
        // "ts,cpu" header auto-detected, first data row is 1000.
        let content = "ts,cpu\n1000,42.5\n2000,55.3\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).expect("should read column 0");
        assert_eq!(gen.value(0), 1000.0);
        assert_eq!(gen.value(1), 2000.0);
    }

    #[test]
    fn multi_column_csv_reads_last_column() {
        // "a,b,c" header auto-detected.
        let content = "a,b,c\n1.0,2.0,3.0\n4.0,5.0,6.0\n";
        let gen = CsvReplayGenerator::from_str(content, 2, true).expect("should read last column");
        assert_eq!(gen.value(0), 3.0);
        assert_eq!(gen.value(1), 6.0);
    }

    // ---- Auto-detection: header skipping -------------------------------------

    #[test]
    fn auto_detect_skips_non_numeric_header() {
        // "timestamp,cpu_percent" has non-numeric "cpu_percent" → header.
        let content = "timestamp,cpu_percent\n1000,42.5\n2000,55.3\n";
        let gen =
            CsvReplayGenerator::from_str(content, 1, true).expect("should auto-detect header");
        assert_eq!(gen.value(0), 42.5);
        assert_eq!(gen.value(1), 55.3);
    }

    #[test]
    fn auto_detect_includes_all_numeric_first_row() {
        // All fields are numeric → no header detected, first row is data.
        let content = "999.0\n100.0\n200.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true)
            .expect("all-numeric first row should be included as data");
        assert_eq!(
            gen.value(0),
            999.0,
            "first value should be 999.0 (not skipped)"
        );
        assert_eq!(gen.value(1), 100.0);
        assert_eq!(gen.value(2), 200.0);
    }

    #[test]
    fn auto_detect_header_after_comments_and_empty_lines() {
        // Comments and empty lines come before the header; header is the first "data" line.
        let content = "# comment\n\nheader\n10.0\n20.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true)
            .expect("header after comments/empty should be skipped");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
    }

    #[test]
    fn auto_detect_multi_column_all_numeric_no_skip() {
        // Multi-column, all fields numeric → not a header.
        let content = "1000,42.5,60.0\n2000,55.3,70.1\n";
        let gen = CsvReplayGenerator::from_str(content, 1, true)
            .expect("all-numeric multi-column first row should be data");
        assert_eq!(gen.value(0), 42.5);
        assert_eq!(gen.value(1), 55.3);
    }

    // ---- Comment lines (#) are skipped ----------------------------------------

    #[test]
    fn comment_lines_are_skipped() {
        let content = "# this is a comment\n1.0\n# another comment\n2.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, true).expect("comments should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
    }

    #[test]
    fn comment_with_leading_whitespace_is_skipped() {
        let content = "  # indented comment\n5.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true)
            .expect("indented comment should be skipped");
        assert_eq!(gen.value(0), 5.0);
    }

    // ---- Empty lines are skipped ----------------------------------------------

    #[test]
    fn empty_lines_are_skipped() {
        let content = "\n1.0\n\n\n2.0\n\n3.0\n";
        let gen =
            CsvReplayGenerator::from_str(content, 0, true).expect("empty lines should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    #[test]
    fn whitespace_only_lines_are_skipped() {
        let content = "   \n1.0\n  \t  \n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true)
            .expect("whitespace-only lines should be skipped");
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
    }

    // ---- repeat=true cycles correctly -----------------------------------------

    #[test]
    fn repeat_true_cycles_at_boundary() {
        let content = "10.0\n20.0\n30.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).expect("should load 3 values");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 10.0, "tick=3 should wrap to index 0");
        assert_eq!(gen.value(4), 20.0, "tick=4 should wrap to index 1");
        assert_eq!(gen.value(5), 30.0, "tick=5 should wrap to index 2");
    }

    #[test]
    fn repeat_true_multiple_full_cycles() {
        let content = "1.0\n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        for cycle in 0..5 {
            assert_eq!(gen.value(cycle * 2), 1.0, "cycle {cycle}: index 0");
            assert_eq!(gen.value(cycle * 2 + 1), 2.0, "cycle {cycle}: index 1");
        }
    }

    // ---- repeat=false clamps to last value ------------------------------------

    #[test]
    fn repeat_false_clamps_to_last_value() {
        let content = "10.0\n20.0\n30.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).expect("should load 3 values");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 30.0, "tick=3 should clamp to last value");
        assert_eq!(gen.value(100), 30.0, "tick=100 should clamp to last value");
    }

    #[test]
    fn repeat_false_at_exact_boundary_returns_last() {
        let content = "5.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).unwrap();
        assert_eq!(gen.value(0), 5.0);
        assert_eq!(
            gen.value(1),
            5.0,
            "single-element, tick=1 should clamp to 5.0"
        );
    }

    // ---- Empty file returns error ---------------------------------------------

    #[test]
    fn empty_file_returns_error() {
        let (_tmp, path) = temp_csv("");
        let result = CsvReplayGenerator::new(&path, 0, true);
        assert!(result.is_err(), "empty file must return an error");
        let err = result.err().expect("already confirmed is_err");
        let msg = format!("{err}");
        assert!(
            msg.contains("no valid numeric values"),
            "error message should mention 'no valid numeric values', got: {msg}"
        );
    }

    #[test]
    fn empty_content_from_str_returns_error() {
        let result = CsvReplayGenerator::from_str("", 0, true);
        assert!(result.is_err(), "empty content must return an error");
    }

    // ---- File with no valid values returns error ------------------------------

    #[test]
    fn file_with_only_comments_returns_error() {
        let content = "# comment 1\n# comment 2\n";
        let result = CsvReplayGenerator::from_str(content, 0, true);
        assert!(result.is_err(), "file with only comments must error");
    }

    #[test]
    fn file_with_only_header_returns_error() {
        // "timestamp,cpu" is auto-detected as header; no data rows follow.
        let content = "timestamp,cpu\n";
        let result = CsvReplayGenerator::from_str(content, 0, true);
        assert!(result.is_err(), "file with only a header row must error");
    }

    #[test]
    fn file_with_no_parseable_numbers_returns_error() {
        // "not_a_number" is auto-detected as header; remaining lines also non-numeric.
        let content = "not_a_number\nhello\nworld\n";
        let result = CsvReplayGenerator::from_str(content, 0, true);
        assert!(result.is_err(), "file with no parseable numbers must error");
    }

    #[test]
    fn file_with_header_and_unparseable_body_returns_error() {
        // "header" is auto-detected as header; "abc" and "def" are not parseable.
        let content = "header\nabc\ndef\n";
        let result = CsvReplayGenerator::from_str(content, 0, true);
        assert!(
            result.is_err(),
            "file with header and no parseable body must error"
        );
    }

    // ---- File not found returns error -----------------------------------------

    #[test]
    fn file_not_found_returns_generator_file_read_error() {
        let result = CsvReplayGenerator::new("/nonexistent/path/that/does/not/exist.csv", 0, true);
        assert!(result.is_err(), "missing file must return an error");
        let err = result.err().expect("already confirmed is_err");
        match err {
            SondaError::Generator(GeneratorError::FileRead {
                ref path,
                ref source,
            }) => {
                assert!(
                    path.contains("does/not/exist.csv"),
                    "FileRead path should contain the file name, got: {path}"
                );
                assert_eq!(
                    source.kind(),
                    std::io::ErrorKind::NotFound,
                    "source io::Error should be NotFound"
                );
            }
            _ => panic!("expected SondaError::Generator(FileRead), got: {err:?}"),
        }
    }

    // ---- Invalid column index (out of bounds) returns error -------------------

    #[test]
    fn column_index_out_of_bounds_returns_error() {
        let content = "1.0,2.0\n3.0,4.0\n";
        // Column 5 does not exist in a 2-column CSV.
        let result = CsvReplayGenerator::from_str(content, 5, true);
        assert!(
            result.is_err(),
            "column index out of bounds must return an error"
        );
    }

    #[test]
    fn column_index_out_of_bounds_on_disk() {
        let (_tmp, path) = temp_csv("1.0,2.0\n3.0,4.0\n");
        let result = CsvReplayGenerator::new(&path, 10, true);
        assert!(
            result.is_err(),
            "column index out of bounds on disk file must error"
        );
    }

    // ---- Large tick values don't panic ----------------------------------------

    #[test]
    fn repeat_large_tick_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        let large_tick: u64 = 1_000_000_000;
        let val = gen.value(large_tick);
        let expected_index = (large_tick % 3) as usize;
        let expected = [1.0, 2.0, 3.0][expected_index];
        assert_eq!(val, expected);
    }

    #[test]
    fn no_repeat_large_tick_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).unwrap();
        let large_tick: u64 = 1_000_000_000;
        assert_eq!(gen.value(large_tick), 3.0, "should clamp to last value");
    }

    // ---- 32-bit truncation safety (tick > u32::MAX) ----------------------------

    #[test]
    fn repeat_tick_above_u32_max_uses_u64_modulo() {
        let content = "10.0\n20.0\n30.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        // tick = 4_294_967_296: u64 modulo 4_294_967_296 % 3 = 1
        let tick: u64 = u64::from(u32::MAX) + 1;
        assert_eq!(
            gen.value(tick),
            20.0,
            "tick {} mod 3 = 1, should return values[1] = 20.0",
            tick
        );
    }

    #[test]
    fn repeat_tick_at_u64_max_does_not_panic() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        let val = gen.value(u64::MAX);
        // u64::MAX % 3 = 0
        assert_eq!(val, 1.0, "u64::MAX % 3 = 0, should return values[0]");
    }

    #[test]
    fn no_repeat_tick_above_u32_max_clamps_correctly() {
        let content = "1.0\n2.0\n3.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).unwrap();
        let tick: u64 = u64::from(u32::MAX) + 1;
        assert_eq!(
            gen.value(tick),
            3.0,
            "tick {} beyond length should clamp to last value",
            tick
        );
    }

    #[test]
    fn no_repeat_tick_at_u64_max_clamps_correctly() {
        let content = "1.0\n2.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).unwrap();
        assert_eq!(
            gen.value(u64::MAX),
            2.0,
            "u64::MAX should clamp to last value"
        );
    }

    // ---- CsvReplayGenerator is Send + Sync ------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn csv_replay_generator_is_send_and_sync() {
        assert_send_sync::<CsvReplayGenerator>();
    }

    // ---- Determinism: same tick always returns same value ---------------------

    #[test]
    fn determinism_same_tick_returns_same_value() {
        let content = "10.0\n20.0\n30.0\n40.0\n50.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        for tick in 0..50 {
            let first_call = gen.value(tick);
            let second_call = gen.value(tick);
            assert_eq!(
                first_call, second_call,
                "value must be deterministic: tick={tick} returned {first_call} then {second_call}"
            );
        }
    }

    #[test]
    fn determinism_separate_instances_same_content() {
        let content = "5.0\n10.0\n15.0\n";
        let gen1 = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        let gen2 = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        for tick in 0..30 {
            assert_eq!(
                gen1.value(tick),
                gen2.value(tick),
                "two generators with same content must produce same values at tick={tick}"
            );
        }
    }

    // ---- Factory creates generator from config --------------------------------

    #[test]
    fn factory_csv_replay_creates_working_generator() {
        let (_tmp, path) = temp_csv("10.0\n20.0\n30.0\n");
        let config = super::super::GeneratorConfig::CsvReplay {
            file: path,
            column: Some(0),
            repeat: Some(true),
            columns: None,
            timescale: None,
            default_metric_name: None,
        };
        let gen =
            super::super::create_generator(&config, 1.0).expect("csv_replay factory must succeed");
        assert_eq!(gen.value(0), 10.0);
        assert_eq!(gen.value(1), 20.0);
        assert_eq!(gen.value(2), 30.0);
        assert_eq!(gen.value(3), 10.0, "should wrap around");
    }

    #[test]
    fn factory_csv_replay_defaults() {
        // column defaults to 0, repeat defaults to true; "header" auto-detected as header.
        let (_tmp, path) = temp_csv("header\n42.0\n");
        let config = super::super::GeneratorConfig::CsvReplay {
            file: path,
            column: None,
            repeat: None,
            columns: None,
            timescale: None,
            default_metric_name: None,
        };
        let gen = super::super::create_generator(&config, 1.0)
            .expect("csv_replay factory with defaults must succeed");
        // "header" is auto-detected as header, so it is skipped, leaving 42.0
        assert_eq!(gen.value(0), 42.0);
    }

    #[test]
    fn factory_csv_replay_missing_file_returns_error() {
        let config = super::super::GeneratorConfig::CsvReplay {
            file: "/nonexistent/file.csv".to_string(),
            column: None,
            repeat: None,
            columns: None,
            timescale: None,
            default_metric_name: None,
        };
        let result = super::super::create_generator(&config, 1.0);
        assert!(
            result.is_err(),
            "factory with missing file must return error"
        );
    }

    // ---- Example YAML deserializes and runs -----------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_csv_replay_config_from_yaml() {
        let yaml = "\
type: csv_replay
file: /some/path.csv
repeat: false
";
        let config: super::super::GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("csv_replay YAML must deserialize");
        match config {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                repeat,
                columns,
                ..
            } => {
                assert_eq!(file, "/some/path.csv");
                assert_eq!(column, None, "column is serde(skip), should be None");
                assert_eq!(columns, None, "columns should be None when omitted");
                assert_eq!(repeat, Some(false));
            }
            _ => panic!("expected CsvReplay variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn deserialize_csv_replay_config_minimal() {
        let yaml = "type: csv_replay\nfile: data.csv\n";
        let config: super::super::GeneratorConfig =
            serde_yaml_ng::from_str(yaml).expect("minimal csv_replay YAML must deserialize");
        match config {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                repeat,
                columns,
                ..
            } => {
                assert_eq!(file, "data.csv");
                assert_eq!(column, None, "column should be None (serde skip)");
                assert_eq!(columns, None, "columns should be None when omitted");
                assert_eq!(repeat, None, "repeat should be None when omitted");
            }
            _ => panic!("expected CsvReplay variant"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn example_yaml_scenario_file_deserializes() {
        // Validate the example file pattern from examples/csv-replay-metrics.yaml.
        // We use a temp CSV to allow the factory to actually load data.
        let (_tmp, csv_path) =
            temp_csv("timestamp,cpu_percent\n1700000000,12.3\n1700000010,14.1\n");
        let yaml = format!(
            "\
name: cpu_replay
rate: 1
duration: 60s

generator:
  type: csv_replay
  file: {}
  columns:
    - index: 1
      name: cpu_replay

labels:
  instance: prod-server-42
  job: node

encoder:
  type: prometheus_text
sink:
  type: stdout
",
            csv_path
        );
        let config: crate::config::ScenarioConfig =
            serde_yaml_ng::from_str(&yaml).expect("example scenario YAML must deserialize");
        assert_eq!(config.name, "cpu_replay");
        assert_eq!(config.rate, 1.0);
        match &config.generator {
            super::super::GeneratorConfig::CsvReplay {
                file,
                column,
                repeat,
                columns,
                ..
            } => {
                assert_eq!(file, &csv_path);
                assert_eq!(*column, None, "column is serde(skip)");
                let cols = columns.as_ref().expect("columns should be Some");
                assert_eq!(cols.len(), 1);
                assert_eq!(cols[0].index, 1);
                assert_eq!(cols[0].name, "cpu_replay");
                assert_eq!(*repeat, None, "repeat not specified");
            }
            _ => panic!("expected CsvReplay generator variant"),
        }

        // After expansion, verify the factory can create a working generator.
        let expanded = crate::config::expand_scenario(config).expect("expand must succeed");
        assert_eq!(expanded.len(), 1);
        let gen = super::super::create_generator(&expanded[0].generator, expanded[0].rate)
            .expect("factory must succeed for expanded config");
        assert_eq!(gen.value(0), 12.3);
        assert_eq!(gen.value(1), 14.1);
    }

    // ---- Unparseable rows are silently skipped --------------------------------

    #[test]
    fn unparseable_rows_are_refused_naming_the_line() {
        // This test used to be called `unparseable_rows_are_skipped` and asserted
        // that "1.0\nnot_a_number\n2.0\n???\n3.0\n" replayed as 1.0, 2.0, 3.0 —
        // i.e. it pinned the defect. Skipping the junk shortened the vector, so
        // 2.0 played at the instant that belonged to `not_a_number` and every
        // later sample moved up with it. A timeline that silently slides is
        // worse than a file that refuses to load.
        let content = "1.0\nnot_a_number\n2.0\n???\n3.0\n";
        let msg = match CsvReplayGenerator::from_str(content, 0, true) {
            Ok(_) => panic!("junk must be refused, not skipped"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("line 2"), "names the offending line: {msg}");
        assert!(msg.contains("not_a_number"), "quotes the cell: {msg}");
    }

    #[test]
    fn a_blank_cell_holds_its_slot_rather_than_shrinking_the_column() {
        // The other half of the same rule: blank means absent, and absent still
        // occupies its instant. Four data rows must yield four slots.
        let content = "1.0\n\n3.0\n";
        let (values, blanks) =
            CsvReplayGenerator::parse_values_and_gaps(content, 0).expect("blank cells are legal");
        assert_eq!(values.len(), 2, "a blank LINE is skipped, not a blank cell");
        assert!(blanks.is_empty(), "no blank cells here — just a blank line");

        // A blank *cell* in a real row is the case that matters.
        let content = "0,1.0\n1,\n2,3.0\n";
        let (values, blanks) =
            CsvReplayGenerator::parse_values_and_gaps(content, 1).expect("blank cells are legal");
        assert_eq!(values.len(), 3, "three rows, three slots");
        assert_eq!(values[0], 1.0);
        assert!(values[1].is_nan(), "the blank holds its slot");
        assert_eq!(values[2], 3.0, "3.0 did not move up into the hole");
        assert_eq!(blanks, vec![1], "row 1 is reported as a gap");
    }

    // ---- first_uncovered_tick ---------------------------------------------
    //
    // The interval walk that replaced enumerating the held tail. It exists for
    // cost — 86.4M ticks took 8 seconds and a ~700 MB Vec — so the cases below
    // pin the answer it has to keep giving, including at the half-open edges
    // where a cheaper formulation would drift from `covered`.

    #[rustfmt::skip]
    #[rstest::rstest]
    // No windows at all: the first tick asked about is the answer.
    #[case::no_windows(5, 9, &[], Some(5))]
    // Fully covered range -> nothing to report.
    #[case::fully_covered(5, 9, &[(5.0, 10.0)], None)]
    // Window ends mid-range: the first tick at or past its end.
    #[case::covers_prefix(5, 9, &[(5.0, 8.0)], Some(8))]
    // Window starts after the cursor: the cursor itself is uncovered.
    #[case::window_starts_later(5, 9, &[(7.0, 12.0)], Some(5))]
    // Half-open at the far edge: a window ending exactly on a tick does NOT
    // cover it, because coverage is `t * step < end`.
    #[case::end_is_exclusive(5, 9, &[(5.0, 9.0)], Some(9))]
    // Two windows with a hole between them.
    #[case::hole_between(0, 9, &[(0.0, 3.0), (5.0, 10.0)], Some(3))]
    // Adjacent windows chain into continuous coverage.
    #[case::adjacent_chain(0, 9, &[(0.0, 5.0), (5.0, 10.0)], None)]
    // Order in the list must not matter — the walk sorts.
    #[case::unsorted_input(0, 9, &[(5.0, 10.0), (0.0, 5.0)], None)]
    // Overlapping windows also chain.
    #[case::overlapping(0, 9, &[(0.0, 6.0), (4.0, 10.0)], None)]
    // Empty range: nothing is held, so nothing escapes.
    #[case::empty_range(9, 8, &[], None)]
    fn first_uncovered_tick_cases(
        #[case] from: usize,
        #[case] to: u64,
        #[case] windows: &[(f64, f64)],
        #[case] expected: Option<usize>,
    ) {
        assert_eq!(first_uncovered_tick(from, to, windows, 1.0), expected);
    }

    /// The window `at: 100ms, for: 200ms` on a 10 Hz capture — ordinary YAML,
    /// not a constructed float.
    ///
    /// `config/mod.rs` builds the window as `(at, at + dur)` in f64, giving
    /// `end = 0.1 + 0.2 = 0.30000000000000004`. Tick 3 sits exactly ON that
    /// end, so it is NOT covered. The walk used to advance with
    /// `ceil(end / step)` = 4, step over tick 3, and return `None` — "nothing
    /// uncovered, config is fine" — so the clamp rule accepted a scenario whose
    /// held silence emits NaN. Found independently by both reviewers; this is
    /// the reviewer's construction, because it is the one a user can type.
    #[test]
    fn a_window_end_that_lands_between_floats_does_not_hide_an_uncovered_tick() {
        let step = 0.1;
        let end = 0.1 + 0.2;
        assert_ne!(end, 0.3, "the premise: at + dur is not the decimal 0.3");
        let windows: &[(f64, f64)] = &[(0.1, end)];

        // The cursor must START inside the window, or the walk never reaches
        // the jump and the case proves nothing — the void probe the reviewer
        // reported against himself.
        assert!(
            tick_in_window(2, step, windows[0]),
            "tick 2 must be covered"
        );
        assert!(!tick_in_window(3, step, windows[0]), "tick 3 sits on `end`");

        assert_eq!(
            first_uncovered_tick(2, 3, windows, step),
            Some(3),
            "tick 3 is uncovered and within range"
        );
    }

    /// The walk must agree with `covered` in BOTH directions: the tick it
    /// reports is uncovered, AND it skipped no earlier one.
    ///
    /// `assert_eq!` against the brute-force `find` is what makes it
    /// two-directional — a walk that returns `None` where the scan finds a tick
    /// fails just as loudly as a wrong tick. The first version of this test was
    /// two-directional already and still missed the defect, because its windows
    /// were round decimals. These are built the way `config/mod.rs` builds
    /// them, `at + dur` in f64, which is where the disagreement lives.
    #[test]
    fn first_uncovered_tick_agrees_with_a_brute_force_scan() {
        let step = 0.1;
        // Every window here is `(at, at + dur)`, the shape config produces.
        let windows: &[(f64, f64)] = &[
            (0.1, 0.1 + 0.2),
            (0.5, 0.5 + 0.1),
            (0.7, 0.7 + 0.30000000000000004),
        ];
        let covered = |t: usize| windows.iter().any(|&w| tick_in_window(t, step, w));
        for from in 0..24usize {
            for to in from..24usize {
                let brute = (from..=to).find(|&t| !covered(t));
                let walked = first_uncovered_tick(from, to as u64, windows, step);
                assert_eq!(
                    walked, brute,
                    "from={from} to={to}: walk said {walked:?}, scan said {brute:?}"
                );
            }
        }
    }

    #[test]
    fn a_row_too_short_for_its_column_is_refused() {
        let content = "0,1.0\n1\n2,3.0\n";
        let msg = match CsvReplayGenerator::from_str(content, 1, true) {
            Ok(_) => panic!("a ragged row must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("line 2"), "names the line: {msg}");
    }

    #[test]
    fn a_literal_nan_cell_is_a_present_sample_not_a_gap() {
        // Prometheus really does return NaN values. They replay as data, and
        // must NOT be reported as gaps — otherwise the cross-check would demand
        // a gap window over a sample that was genuinely recorded.
        let content = "0,1.0\n1,NaN\n2,3.0\n";
        let (values, blanks) =
            CsvReplayGenerator::parse_values_and_gaps(content, 1).expect("NaN is a legal value");
        assert!(values[1].is_nan());
        assert!(
            blanks.is_empty(),
            "a literal NaN is present data, not an absent sample"
        );
    }

    // ---- Mixed: comments, empty lines, header, unparseable --------------------

    #[test]
    fn mixed_content_loads_correctly() {
        let content = "\
# CPU values from production
# Exported 2024-01-15

timestamp,cpu_percent
1700000000,12.3

# spike starts here
1700000010,
1700000020,95.5

";
        let gen =
            CsvReplayGenerator::from_str(content, 1, true).expect("mixed content should load");
        // Comments, blank lines and the header are still skipped. The middle
        // row's cell is now blank rather than "bad_data": it holds its slot, so
        // 95.5 stays at index 2 instead of moving up to index 1. That shift is
        // the defect this file used to encode.
        assert_eq!(gen.value(0), 12.3);
        assert!(gen.value(1).is_nan(), "the blank holds index 1");
        assert_eq!(gen.value(2), 95.5, "95.5 did not move up");
        // The cycle wraps at 3 now, not 2 — the blank is a slot, so the column
        // is one longer than it was when a bad cell vanished from it.
        assert_eq!(gen.value(3), 12.3, "should cycle");
    }

    // ---- Fields with whitespace trim correctly --------------------------------

    #[test]
    fn fields_with_whitespace_are_trimmed() {
        let content = "  1.0  ,  2.0  \n  3.0  ,  4.0  \n";
        let gen = CsvReplayGenerator::from_str(content, 1, true)
            .expect("whitespace around fields should be trimmed");
        assert_eq!(gen.value(0), 2.0);
        assert_eq!(gen.value(1), 4.0);
    }

    // ---- repeat defaults to true ----------------------------------------------

    #[test]
    fn repeat_defaults_to_true_via_factory() {
        let (_tmp, path) = temp_csv("1.0\n2.0\n");
        let config = super::super::GeneratorConfig::CsvReplay {
            file: path,
            column: Some(0),
            repeat: None,
            columns: None,
            timescale: None,
            default_metric_name: None,
        };
        let gen = super::super::create_generator(&config, 1.0).expect("factory must succeed");
        // With repeat defaulting to true, tick=2 on a 2-element seq should wrap.
        assert_eq!(gen.value(2), 1.0, "repeat=None should default to true");
    }

    // ---- Single value file ----------------------------------------------------

    #[test]
    fn single_value_repeat_true() {
        let content = "42.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(1), 42.0);
        assert_eq!(gen.value(100), 42.0);
    }

    #[test]
    fn single_value_repeat_false() {
        let content = "42.0\n";
        let gen = CsvReplayGenerator::from_str(content, 0, false).unwrap();
        assert_eq!(gen.value(0), 42.0);
        assert_eq!(gen.value(1), 42.0);
        assert_eq!(gen.value(100), 42.0);
    }

    // ---- Negative and special float values ------------------------------------

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 is a sample CSV value, not the PI constant
    fn handles_negative_values() {
        let content = "-1.5\n-2.5\n0.0\n3.14\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        assert_eq!(gen.value(0), -1.5);
        assert_eq!(gen.value(1), -2.5);
        assert_eq!(gen.value(2), 0.0);
        assert_eq!(gen.value(3), 3.14);
    }

    #[test]
    fn handles_integer_values() {
        let content = "1\n2\n3\n";
        let gen = CsvReplayGenerator::from_str(content, 0, true).unwrap();
        assert_eq!(gen.value(0), 1.0);
        assert_eq!(gen.value(1), 2.0);
        assert_eq!(gen.value(2), 3.0);
    }

    // ---- Verify value count ---------------------------------------------------

    #[test]
    fn correct_number_of_values_loaded() {
        // The sample CSV has 5 data rows + 1 header + 3 comment lines.
        // column 1 = val. Header auto-detected (non-numeric "val").
        // Comments are skipped. All 5 data rows should parse.
        let content = "\
# comment 1
# comment 2
# comment 3
ts,val
1,10.0
2,20.0
3,30.0
4,40.0
5,50.0
";
        let gen = CsvReplayGenerator::from_str(content, 1, true).expect("should load 5 values");
        // Verify wrapping at length 5
        assert_eq!(gen.value(5), gen.value(0), "should wrap at 5 values");
        assert_eq!(gen.value(6), gen.value(1));
    }

    // ---- Regression: sample-cpu-values.csv loads correctly --------------------

    #[test]
    fn sample_cpu_values_csv_from_disk() {
        // This test uses the actual example file shipped with the project.
        // It validates the end-to-end path: file -> parse -> generator.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/sample-cpu-values.csv"
        );
        let result = CsvReplayGenerator::new(path, 1, true);
        match result {
            Ok(gen) => {
                // First data row: 1700000000,12.3
                assert!(
                    (gen.value(0) - 12.3).abs() < 1e-10,
                    "first value should be 12.3, got {}",
                    gen.value(0)
                );
                // Values should cycle: 50 data rows
                assert_eq!(
                    gen.value(50),
                    gen.value(0),
                    "should wrap at 50 values (tick 50 == tick 0)"
                );
            }
            Err(e) => {
                // If the file is not at the expected path (CI environment),
                // skip gracefully. The from_str tests cover the logic.
                eprintln!("Skipping sample CSV disk test (file not found): {e}");
            }
        }
    }
}
