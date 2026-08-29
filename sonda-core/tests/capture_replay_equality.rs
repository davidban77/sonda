//! One capture, from the wire to the emitted bytes, compared against the file
//! the importer wrote.
//!
//! Each half of this path is already tested and none of the tests meet:
//! `acquire_capture` stops once the artifacts load, `csv_replay_roundtrip`
//! reads values straight out of the generator with no clock, and
//! `gap_window_alignment` drives the real loop but on a synthetic ladder and
//! asserts which ticks fired, not what they carried. Nothing asserted that a
//! recording replays as the numbers that were recorded.
//!
//! # The comparison
//!
//! Emissions are paired with rows **by order** — ticks emit strictly
//! increasing, so the i-th emission is the i-th expected row. Values compare
//! exactly, with `NaN` equal to `NaN` because a literal `NaN` cell is a sample
//! the database reported. Instants are asserted **one-sided**: an emission may
//! never carry a timestamp earlier than its row's slot, and no upper bound is
//! placed on lateness.
//!
//! Never-early is free of tolerance because the loop guarantees it by
//! construction — `emit_at = next_deadline.max(now)` — so a replay running too
//! fast violates it on any machine, while a loaded host only ever makes an
//! emission late. Bounding lateness instead would gate the documented
//! degradation of a busy runner, which is how a test becomes a weather report.
//!
//! # Red-verifying never-early
//!
//! No mutation of the written artifacts reaches this assertion — the three
//! conformance checks (no `start_time:`, `duration:` equals rows x step, the
//! count) catch every artifact-level way to make a run play early. It answers
//! only to a regression in the timestamp path, so proving it fires means
//! sabotaging that path. In `core_loop::WallClock::wall_at`:
//!
//! ```text
//! -    self.base + scenario_elapsed
//! +    self.base + scenario_elapsed.mul_f64(0.5)
//! ```
//!
//! All six cases then fail. The instants are wall-clock and differ per run;
//! what reproduces is the row and the margin — half a 250ms step:
//!
//! ```text
//! emission 1 (data row 1) is stamped <t>ms, 125ms before its slot at <t+125>ms.
//! A row can play late; playing early means the replay ran fast.
//! ```
//!
//! # The known exposure
//!
//! `core_loop` ends a run when the *emission* instant passes `duration:`, so a
//! host that falls a full interval behind drops the last tick and the count
//! assertion fails for an infrastructure reason. That needs a 250ms stall here;
//! `gap_window_alignment` already ships a ±70ms wall assertion on the same
//! loop. The count panic names both readings so a stall is not mistaken for a
//! defect.

#![cfg(all(feature = "http", feature = "config"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sonda_core::acquire::normalize::{normalize, Grid};
use sonda_core::acquire::tsdb::{Auth, TsdbClient};
use sonda_core::acquire::{csv_out, yaml_out};
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::{expand_entry, ScenarioEntry};
use sonda_core::schedule::runner;
use sonda_core::sink::Sink;
use sonda_core::{CancellationToken, SondaError};

/// The capture geometry every case but the last one runs on.
///
/// The database is sampled at 1s and the scenario carries `timescale: 4`, so a
/// row plays every 250ms. 0.25 is exactly representable and every multiple is a
/// whole number of milliseconds, so the expected-instant arithmetic below is
/// exact in both `f64` and the integer-millisecond exposition timestamps.
const FILE_STEP_SECS: f64 = 1.0;
const TIMESCALE: f64 = 4.0;

/// Serve `body` to exactly one request.
fn mock_tsdb(body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap_or(0) == 1 {
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    base
}

/// One emitted exposition line, reduced to what the comparison needs.
///
/// The timestamp is parsed out of the line rather than taken from the sink's
/// arrival clock: the claim under test is that the *artifact a consumer parses*
/// carries truthful instants.
#[derive(Debug, Clone, Copy)]
struct Emitted {
    value: f64,
    ts_ms: u64,
}

struct LineSink {
    seen: Arc<Mutex<Vec<Emitted>>>,
}

#[async_trait::async_trait]
impl Sink for LineSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let text = std::str::from_utf8(data).expect("prometheus output is valid UTF-8");
        let mut seen = self.seen.lock().expect("line sink mutex poisoned");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            // `metric{labels} <value> <timestamp_ms>`
            let mut fields = line.rsplit(' ');
            let ts_ms = fields
                .next()
                .and_then(|t| t.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("line has no parseable timestamp: {line}"));
            let value = fields
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(|| panic!("line has no parseable value: {line}"));
            seen.push(Emitted { value, ts_ms });
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

/// A capture as the fixture describes it: one entry per grid point, `None`
/// where the database had nothing.
///
/// The values are the literal text the server returns, so a case can serve
/// `"NaN"` and keep it distinct from an absent point.
struct Capture {
    slots: &'static [Option<&'static str>],
    timescale: f64,
}

impl Capture {
    fn file_step(&self) -> f64 {
        FILE_STEP_SECS
    }

    /// Seconds between rows on playback — what `timescale:` divides.
    fn play_step(&self) -> f64 {
        self.file_step() / self.timescale
    }

    fn present(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// What one capture produced, kept together so assertions can name any of it.
struct Artifacts {
    csv_text: String,
    yaml: String,
    /// Values the mock served, in grid order, as the text it sent.
    served: Vec<Option<&'static str>>,
    _dir: tempfile::TempDir,
}

/// Fetch, normalize and write both artifacts, asserting provenance as it goes.
///
/// The guards here tie the written CSV to what the server sent. Without them a
/// broken writer and a broken reader could agree with each other and the
/// pairwise comparison downstream would pass on two wrong halves.
fn capture(cap: &Capture) -> Artifacts {
    let samples: Vec<String> = cap
        .slots
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| format!(r#"[{},"{v}"]"#, i as f64 * cap.file_step())))
        .collect();
    assert!(
        !samples.is_empty(),
        "a fixture that serves no points would make every downstream assertion vacuous"
    );
    // Pairing is by order, so two rows sharing a value would let a swap between
    // them pass. Checked rather than left as a convention the next fixture has
    // to remember, and checked with `eq` — the same predicate the pairwise
    // comparison uses. Deduping the text instead would call "48" and "48.0"
    // distinct while the comparison they guard calls them equal.
    let parsed: Vec<f64> = cap
        .slots
        .iter()
        .flatten()
        .map(|s| s.parse().expect("the fixture serves numbers"))
        .collect();
    for (i, a) in parsed.iter().enumerate() {
        assert!(
            !parsed[i + 1..].iter().any(|b| eq(*a, *b)),
            "fixture values must be pairwise distinct: {:?}",
            cap.slots
        );
    }
    let body = format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[
             {{"metric":{{"__name__":"up","job":"api"}},"values":[{}]}}]}}}}"#,
        samples.join(",")
    );

    let base = mock_tsdb(&body);
    let end = (cap.slots.len() - 1) as f64 * cap.file_step();
    let series = TsdbClient::new(&base, Auth::None, Duration::from_secs(5))
        .fetch_range("up", 0.0, end, Duration::from_secs_f64(cap.file_step()))
        .expect("fetch must succeed");
    assert_eq!(series.len(), 1, "the fixture serves exactly one series");

    let grid = Grid::new(0.0, end, cap.file_step()).expect("grid");
    assert_eq!(
        grid.len,
        cap.slots.len(),
        "the grid covers every fixture slot"
    );

    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();
    assert_eq!(
        normalized[0].values.len() - normalized[0].gap_count(),
        cap.present(),
        "every point the mock served survived normalization"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let csv_path = dir.path().join("capture.csv");
    let csv_text = csv_out::write_csv(grid, &normalized).expect("csv");
    std::fs::write(&csv_path, &csv_text).expect("write csv");

    let file = yaml_out::scenario_for(
        csv_path.to_str().expect("utf8"),
        grid,
        &normalized,
        cap.timescale,
    )
    .expect("scenario");
    let yaml = yaml_out::to_yaml(&file).expect("yaml");

    Artifacts {
        csv_text,
        yaml,
        served: cap.slots.to_vec(),
        _dir: dir,
    }
}

/// The rows the recording says should play, read back out of the written CSV
/// with the reader the runtime itself uses.
///
/// Returns `(row_index, value)` for every non-blank row. Driving
/// `column_values_and_gaps` rather than a test-side CSV parser is what stops
/// this from becoming a transcription that diverges on the line a bug is on.
fn expected_rows(art: &Artifacts) -> Vec<(usize, f64)> {
    let (values, blanks) =
        sonda_core::generator::csv_replay::column_values_and_gaps(&art.csv_text, 1)
            .expect("the written CSV must read back");

    assert_eq!(
        values.len(),
        art.served.len(),
        "the CSV holds one row per grid point"
    );
    let served_blanks: Vec<usize> = art
        .served
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.is_none().then_some(i))
        .collect();
    assert_eq!(
        blanks, served_blanks,
        "the CSV's blank rows are the points the mock left out"
    );

    // Every written cell equals the text the server sent for it. This ties the
    // writer to the source; everything downstream is read back out of this same
    // file, so a defect the writer and the reader share would otherwise agree
    // with itself. One row is not enough: a writer that rounded every value
    // survived a single-row anchor, because each fixture's first present value
    // is a whole number.
    for (i, served) in art.served.iter().enumerate() {
        let Some(served_text) = served else { continue };
        let want: f64 = served_text.parse().expect("the fixture serves numbers");
        assert!(
            eq(values[i], want),
            "row {i} of the written CSV is {}, but the mock served {served_text}",
            values[i]
        );
    }

    let rows: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter(|(i, _)| !blanks.contains(i))
        .map(|(i, v)| (i, *v))
        .collect();
    assert!(
        !rows.is_empty(),
        "a recording with no present rows would make the pairwise comparison vacuous"
    );
    rows
}

/// Exact, with `NaN` equal to `NaN` — a literal `NaN` cell is a reported
/// sample, not a mistake, and must survive the round trip as one.
fn eq(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// Load the written artifacts and run them, returning what reached the sink and
/// the instant the run was started from.
///
/// The YAML contributes nothing to the expectations — that asymmetry is what
/// lets a mutation to its `rate:` fail rather than move the expectation with
/// it.
async fn replay(art: &Artifacts, cap: &Capture) -> (Vec<Emitted>, SystemTime) {
    let entries = sonda_core::compile_scenario_file(&art.yaml, &InMemoryPackResolver::default())
        .unwrap_or_else(|e| panic!("the compiler rejected the capture: {e}\n{}", art.yaml));
    let mut expanded = Vec::new();
    for e in entries {
        expanded.extend(
            expand_entry(e)
                .unwrap_or_else(|e| panic!("the emitted columns must expand: {e}\n{}", art.yaml)),
        );
    }
    assert_eq!(expanded.len(), 1, "single-series fixture, one scenario");

    let ScenarioEntry::Metrics(config) = expanded.remove(0) else {
        panic!("a metrics capture must expand to a metrics scenario");
    };

    // Conformance: the geometry this gate's arithmetic assumes is the geometry
    // the importer writes. Both are read off the artifact rather than trusted.
    assert!(
        config.base.start_time.is_none(),
        "a capture that set start_time: would shift every emitted instant away \
         from the run's own clock, and the never-early assertion would no longer \
         mean what it says"
    );
    let duration = config
        .base
        .duration
        .as_deref()
        .expect("captures set duration");
    let want_duration = cap.slots.len() as f64 * cap.play_step();
    assert_eq!(
        duration,
        format!("{want_duration}s"),
        "duration must be rows x playback step; with repeat: false a longer run \
         holds the last slot and a shorter one truncates the recording"
    );

    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut sink: Box<dyn Sink> = Box::new(LineSink {
        seen: Arc::clone(&seen),
    });

    // Taken strictly before the run so the loop's own anchor resolves at or
    // after it, which is what makes the never-early inequality safe.
    let t_call = SystemTime::now();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let out = seen.lock().expect("line sink mutex poisoned").clone();
    (out, t_call)
}

/// Compare what came out against what the recording says, in that order:
/// count first, then values and instants pairwise.
fn assert_round_trip(
    seen: &[Emitted],
    expected: &[(usize, f64)],
    t_call: SystemTime,
    cap: &Capture,
) {
    if seen.len() != expected.len() {
        let spacing: Vec<i64> = seen
            .windows(2)
            .map(|w| w[1].ts_ms as i64 - w[0].ts_ms as i64)
            .collect();
        panic!(
            "expected {} emissions, got {}. Gaps between consecutive emissions, in ms: \
             {spacing:?}. All {:.0} means the replay ran on its own grid and the host dropped \
             the tail — see the known exposure in the module docs. Any other spacing is a \
             replay defect.",
            expected.len(),
            seen.len(),
            cap.play_step() * 1000.0,
        );
    }

    let base_ms = t_call
        .duration_since(UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_millis() as u64;

    for (i, (got, (row, want))) in seen.iter().zip(expected.iter()).enumerate() {
        assert!(
            eq(got.value, *want),
            "emission {i} (data row {row}) carried {}, the recording says {want}",
            got.value
        );

        let slot_ms = (*row as f64 * cap.play_step() * 1000.0) as u64;
        assert!(
            got.ts_ms >= base_ms + slot_ms,
            "emission {i} (data row {row}) is stamped {}ms, {}ms before its slot at {}ms. \
             A row can play late; playing early means the replay ran fast.",
            got.ts_ms,
            base_ms + slot_ms - got.ts_ms,
            base_ms + slot_ms,
        );
    }
}

async fn round_trip(cap: Capture) {
    let art = capture(&cap);
    let expected = expected_rows(&art);
    let (seen, t_call) = replay(&art, &cap).await;
    assert_round_trip(&seen, &expected, t_call, &cap);
}

const fn v(s: &'static str) -> Option<&'static str> {
    Some(s)
}
const NONE: Option<&'static str> = None;

/// The seam itself: every row present, every value distinct.
///
/// Distinct values are a rule of these fixtures, not a convenience. Pairing is
/// by order, so two rows sharing a value would let a swap between them pass.
const DENSE: &[Option<&str>] = &[
    v("41"),
    v("42.5"),
    v("43.25"),
    v("44"),
    v("45.5"),
    v("46"),
    v("47.75"),
    v("48"),
    v("49.5"),
    v("50"),
];

#[tokio::test]
async fn a_dense_capture_replays_every_recorded_value_in_order() {
    round_trip(Capture {
        slots: DENSE,
        timescale: TIMESCALE,
    })
    .await;
}

/// Silence mid-recording: the blank rows emit nothing, and the first row after
/// the silence still carries its own slot rather than moving up into the space.
const MID_SILENCE: &[Option<&str>] = &[
    v("11"),
    v("12.5"),
    v("13"),
    NONE,
    NONE,
    v("16.25"),
    v("17"),
    v("18.5"),
    v("19"),
    v("20.75"),
];

#[tokio::test]
async fn silence_inside_a_capture_suppresses_only_its_own_rows() {
    round_trip(Capture {
        slots: MID_SILENCE,
        timescale: TIMESCALE,
    })
    .await;
}

/// A recording that ends in silence. With `repeat: false` the final slot is
/// held for every later tick, so this is the case where the importer's window
/// has to reach the end of the run — driven from the wire rather than from a
/// hand-written config.
const TRAILING_SILENCE: &[Option<&str>] = &[v("7"), v("8.5"), NONE, NONE];

#[tokio::test]
async fn a_capture_ending_in_silence_emits_nothing_after_its_last_sample() {
    round_trip(Capture {
        slots: TRAILING_SILENCE,
        timescale: TIMESCALE,
    })
    .await;
}

/// A literal `NaN` is a sample the database reported, and replays as data.
///
/// The pair to the silence cases: both are `f64::NAN` in the values vector and
/// only the recorded blanks are silence.
const WITH_NAN: &[Option<&str>] = &[v("31"), v("32.5"), v("NaN"), v("34"), v("35.25"), v("36")];

#[tokio::test]
async fn a_recorded_nan_replays_as_a_sample_not_as_silence() {
    round_trip(Capture {
        slots: WITH_NAN,
        timescale: TIMESCALE,
    })
    .await;
}

/// A capture taken mid-outage: row 0 is blank, so the importer writes a window
/// at `0s` and the run opens silent.
const LEADING_SILENCE: &[Option<&str>] = &[NONE, v("22"), v("23.5"), v("24"), v("25.25"), v("26")];

#[tokio::test]
async fn silence_at_the_first_row_delays_the_first_emission_to_its_own_slot() {
    round_trip(Capture {
        slots: LEADING_SILENCE,
        timescale: TIMESCALE,
    })
    .await;
}

/// The undilated path. `timescale: 4` is the geometry every case above runs on,
/// which makes 1 the variant worth its own case.
const UNDILATED: &[Option<&str>] = &[v("61"), v("62.5"), v("63")];

#[tokio::test]
async fn an_undilated_capture_replays_at_the_recorded_step() {
    round_trip(Capture {
        slots: UNDILATED,
        timescale: 1.0,
    })
    .await;
}
