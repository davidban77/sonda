//! The tick a gap suppresses must be the tick that would have played *inside*
//! the gap.
//!
//! `core_loop` sleeps until a tick's deadline before emitting it. If the gap
//! question is asked before that sleep, it is asked about the previous
//! instant, and the first tick of every window escapes: it is decided against
//! an elapsed time one interval too early, finds itself outside the window,
//! and emits.
//!
//! For `gaps:` that is cosmetic — nothing asserts which tick lands where. For
//! csv_replay it is fatal, because the whole replay contract is "row *n* plays
//! at instant *n* × step". A row that plays one slot early crosses into or out
//! of a declared silent window, and the blank cell that window exists to cover
//! emits anyway.
//!
//! Both gap kinds are checked here because both are decided by the same
//! branch, so a fix to one that missed the other would be invisible.

#![cfg(feature = "config")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sonda_core::config::{BaseScheduleConfig, GapConfig, GapWindowConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::runner;
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{CancellationToken, OnSinkError, SondaError};

/// One captured emission: the value the generator produced and how long after
/// the sink was built it reached the sink.
#[derive(Debug, Clone, Copy)]
struct Emission {
    value: f64,
    at: Duration,
}

/// A sink that timestamps every write against its own construction instant.
///
/// The runner builds its schedule and starts its clock immediately after this
/// is constructed, so `at` tracks the loop's `elapsed` to within the setup
/// cost — tens of microseconds, three orders of magnitude below the 200ms
/// grid these tests use.
struct TimingSink {
    start: Instant,
    seen: Arc<Mutex<Vec<Emission>>>,
}

#[async_trait::async_trait]
impl Sink for TimingSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let at = self.start.elapsed();
        let text = std::str::from_utf8(data).expect("prometheus output is valid UTF-8");
        let mut seen = self.seen.lock().expect("timing sink mutex poisoned");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            // `metric{labels} <value> <timestamp_ms>` — the value is the
            // second-from-last whitespace-separated token.
            let mut fields = line.rsplit(' ');
            let _timestamp = fields.next();
            let value = fields
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(|| panic!("line has no parseable value: {line}"));
            seen.push(Emission { value, at });
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

/// A [`TimingSink`] that blocks for `stall` after its `stall_after`-th write.
///
/// Models the one input that moves a played instant and cannot be refused at
/// config time: a sink that stops accepting for a while. Everything else that
/// shifts a tick relative to its window — `repeat`, `bursts:`, a blank last
/// row — is caught by the cross-check before the run starts.
struct StallingSink {
    inner: TimingSink,
    writes: usize,
    stall_after: usize,
    stall: Duration,
}

#[async_trait::async_trait]
impl Sink for StallingSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.inner.write(data).await?;
        self.writes += 1;
        if self.writes == self.stall_after {
            tokio::time::sleep(self.stall).await;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn stalling_sink(
    stall_after: usize,
    stall: Duration,
) -> (Box<dyn Sink>, Arc<Mutex<Vec<Emission>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink: Box<dyn Sink> = Box::new(StallingSink {
        inner: TimingSink {
            start: Instant::now(),
            seen: Arc::clone(&seen),
        },
        writes: 0,
        stall_after,
        stall,
    });
    (sink, seen)
}

fn timing_sink() -> (Box<dyn Sink>, Arc<Mutex<Vec<Emission>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink: Box<dyn Sink> = Box::new(TimingSink {
        start: Instant::now(),
        seen: Arc::clone(&seen),
    });
    (sink, seen)
}

/// The grid this whole file runs on: 5 events per second, one every 200ms.
const STEP: Duration = Duration::from_millis(200);

/// How far an emission may sit from its grid instant before the test fails.
///
/// A tick that landed on the *wrong* slot is off by a full 200ms step, so this
/// only has to be comfortably below one step to stay discriminating while
/// absorbing scheduler wakeup latency.
const SLACK_MS: f64 = 70.0;

/// A scenario whose generator makes every tick identifiable: tick *n* emits
/// the value *n*, up to the length of the sequence.
///
/// `repeat: false` clamps past the end rather than wrapping, so a tick beyond
/// the list is still distinguishable from an early one.
fn ladder_scenario(
    duration: &str,
    gaps: Option<GapConfig>,
    gap_windows: Option<Vec<GapWindowConfig>>,
) -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name: "ladder".to_string(),
            rate: 5.0,
            duration: Some(duration.to_string()),
            gaps,
            gap_windows,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Warn,
        },
        generator: GeneratorConfig::Sequence {
            values: (0..32).map(|n| n as f64).collect(),
            repeat: Some(false),
        },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    }
}

/// Assert the exact set of ticks that played, and that each played on its own
/// grid slot.
///
/// Both halves matter: the value set alone would accept a run where every
/// event fired at once, and the instants alone would accept a run that skipped
/// the right number of events at the wrong offsets.
fn assert_ladder(seen: &[Emission], expected_ticks: &[u64]) {
    // Refuse an empty expectation before comparing against one.
    //
    // Every call site today passes a non-empty ladder, so this is latent
    // rather than live — but `assert_eq!(vec![], vec![])` is how a case
    // written for a run that emits nothing would report success, and this is
    // the one corpus in the repo that was not asserting it found something.
    assert!(
        !expected_ticks.is_empty(),
        "assert_ladder was given an empty expectation — a run that emitted nothing \
         would satisfy it. Assert the absence directly instead."
    );

    let values: Vec<f64> = seen.iter().map(|e| e.value).collect();
    let expected: Vec<f64> = expected_ticks.iter().map(|t| *t as f64).collect();
    assert_eq!(
        values, expected,
        "wrong ticks played: got {values:?}, expected {expected:?}"
    );

    for e in seen {
        let slot = STEP.mul_f64(e.value);
        let drift_ms = e.at.as_secs_f64() * 1000.0 - slot.as_secs_f64() * 1000.0;
        assert!(
            drift_ms.abs() < SLACK_MS,
            "tick {} played {:.1}ms from its grid slot {:?} (allowed ±{SLACK_MS}ms)",
            e.value,
            drift_ms,
            slot,
        );
    }
}

/// A one-shot window covering ticks 1 and 2 must suppress *both*, and tick 3
/// must resume on its own slot rather than immediately after the silence.
///
/// Window `[200ms, 600ms)` on a 200ms grid: ticks 1 (200ms) and 2 (400ms) fall
/// inside; tick 3 (600ms) is the first instant outside. The 1.3s duration ends
/// between slots so the tail of the run cannot be confused with a boundary
/// effect.
#[tokio::test]
async fn a_one_shot_window_suppresses_every_tick_inside_it() {
    let config = ladder_scenario(
        "1.3s",
        None,
        Some(vec![GapWindowConfig {
            at: "200ms".to_string(),
            r#for: "400ms".to_string(),
        }]),
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 3, 4, 5, 6]);
}

/// The same question for the recurring form, decided by the same branch.
///
/// `every: 600ms, for: 400ms` puts the silence at the end of each cycle —
/// `[200ms, 600ms)`, then `[800ms, 1200ms)`. Ticks 1, 2, 4 and 5 fall inside;
/// 0, 3 and 6 do not.
#[tokio::test]
async fn a_recurring_gap_suppresses_every_tick_inside_it() {
    let config = ladder_scenario(
        "1.3s",
        Some(GapConfig {
            every: "600ms".to_string(),
            r#for: "400ms".to_string(),
        }),
        None,
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 3, 6]);
}

/// With no silence at all, every tick on the grid plays on its own slot.
///
/// This is the control: it fails if the grid itself drifts, so a green gap
/// test cannot be credited to a scheduler that is uniformly late.
#[tokio::test]
async fn an_ungapped_run_plays_every_slot() {
    let config = ladder_scenario("1.3s", None, None);

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 1, 2, 3, 4, 5, 6]);
}

/// Two adjacent windows are one silence, and the tick after them is still the
/// tick that belongs at that instant.
///
/// `[200ms, 400ms)` then `[400ms, 800ms)` suppress ticks 1, 2 and 3. Tick 4
/// resumes at 800ms — its own slot, not "wherever the sleep happened to end".
#[tokio::test]
async fn adjacent_windows_resume_on_the_grid() {
    let config = ladder_scenario(
        "1.3s",
        None,
        Some(vec![
            GapWindowConfig {
                at: "200ms".to_string(),
                r#for: "200ms".to_string(),
            },
            GapWindowConfig {
                at: "400ms".to_string(),
                r#for: "400ms".to_string(),
            },
        ]),
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 4, 5, 6]);
}

/// A window that starts at zero suppresses the very first tick.
///
/// The first iteration is the one case where no deadline sleep happens at all,
/// so it exercises the gap question on a path the others do not.
#[tokio::test]
async fn a_window_at_zero_suppresses_the_first_tick() {
    let config = ladder_scenario(
        "1.3s",
        None,
        Some(vec![GapWindowConfig {
            at: "0s".to_string(),
            r#for: "400ms".to_string(),
        }]),
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[2, 3, 4, 5, 6]);
}

/// A window whose end does not land on a grid slot must not resurrect the tick
/// it was still covering.
///
/// `[200ms, 500ms)` on a 200ms grid covers ticks 1 and 2 — tick 2's slot is
/// 400ms, inside the window. The silence ends at 500ms, and the loop resumes by
/// re-deriving the tick from elapsed time: `floor(500ms / 200ms)` is 2. So the
/// arithmetic points back at a tick the window had already suppressed, and
/// emitting it would put a sample inside a declared silence — at 500ms, an
/// instant that is not on the grid either.
///
/// Every other case in this file ends its windows on a slot, where truncation
/// and the window boundary agree. Both reviewers flagged this re-anchor as the
/// one boundary neither round had verified; this is the shape that separates
/// them.
#[tokio::test]
async fn a_window_ending_off_grid_does_not_replay_the_tick_it_covered() {
    let config = ladder_scenario(
        "1.3s",
        None,
        Some(vec![GapWindowConfig {
            at: "200ms".to_string(),
            r#for: "300ms".to_string(),
        }]),
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 3, 4, 5, 6]);
}

/// A window past the end of the run suppresses nothing.
///
/// Guards the direction the other cases cannot: a scheduler that treated any
/// declared window as "silent from here on" would pass every test above.
#[tokio::test]
async fn a_window_after_the_run_suppresses_nothing() {
    let config = ladder_scenario(
        "1.3s",
        None,
        Some(vec![GapWindowConfig {
            at: "10s".to_string(),
            r#for: "1s".to_string(),
        }]),
    );

    let (mut sink, seen) = timing_sink();
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    assert_ladder(&seen, &[0, 1, 2, 3, 4, 5, 6]);
}

/// A stalled sink must not make a tick answer for a window its own slot never
/// entered.
///
/// This settles the one open question from the adversarial review. Gap windows
/// are evaluated in `elapsed`, and `elapsed` is `max(next_deadline, now)`,
/// which jumps to real time while the loop is catching up. So during catch-up
/// a tick is judged at an instant later than its own slot — and if a window
/// has opened in between, the tick is suppressed for a silence that belongs to
/// a later row.
///
/// The window here is `[600ms, 1000ms)`, covering ticks 3 and 4. The sink
/// blocks for 600ms after the second write, so the loop returns around 800ms
/// with ticks 2 onward still owed. Tick 2's slot is 400ms — comfortably
/// outside the window — but it is reached while `elapsed` reads ~800ms, which
/// is inside it.
///
/// For `gaps:` this is a cosmetic misattribution. For csv_replay it is the
/// failure the whole cross-check exists to prevent, arriving from the one
/// direction config validation cannot see: the file and the windows agree, the
/// scenario is accepted, and a row is dropped anyway because the sink was slow.
/// ROW FRAME: a stalled sink may delay a recorded sample, never delete one.
///
/// `gap_windows:` are judged against the tick's own grid slot, so a slow sink
/// cannot make a row answer for a window its slot never entered.
///
/// The window is `[600ms, 1000ms)`, covering ticks 3 and 4. The sink blocks for
/// 600ms after the second write, so the loop returns around 800ms with tick 2
/// still owed. Tick 2's slot is 400ms — outside the window — and in wall frame
/// it was reached while `elapsed` read ~800ms and was **deleted**, which is the
/// measurement that produced the ruling. In row frame it is emitted late.
///
/// Ticks 3 and 4 stay suppressed: their slots really are inside the window, and
/// backpressure is not a reason to resurrect a silence the capture recorded.
#[tokio::test]
async fn a_stalled_sink_does_not_make_a_tick_answer_for_a_later_window() {
    let config = ladder_scenario(
        "1.6s",
        None,
        Some(vec![GapWindowConfig {
            at: "600ms".to_string(),
            r#for: "400ms".to_string(),
        }]),
    );

    let (mut sink, seen) = stalling_sink(2, Duration::from_millis(600));
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    let played: Vec<u64> = seen.iter().map(|e| e.value as u64).collect();

    // Only the tick set is asserted. Instants are deliberately not checked:
    // after a stall the loop is behind by construction, so drift from the grid
    // is expected and is not what this is about.
    assert!(
        played.contains(&2),
        "tick 2's slot (400ms) is outside the window [600ms, 1000ms), so the stall \
         must not suppress it; played {played:?}"
    );
    assert!(
        !played.contains(&3) && !played.contains(&4),
        "ticks 3 and 4 sit inside the window and must stay suppressed; played {played:?}"
    );
}

/// Does the divergence stay bounded by the stall, or accumulate across stalls?
///
/// The adversarial reviewer's question, and it decides whether the honest
/// resolution is a documented degradation or a re-anchor on the catch-up path.
/// No window at all here — this measures only how far each tick's emission
/// instant sits from its own grid slot, before and after two separate stalls.
#[tokio::test]
async fn stall_drift_is_bounded_by_the_stall_and_does_not_accumulate() {
    let config = ladder_scenario("2.4s", None, None);

    // Two stalls of 400ms each, at the 2nd and 7th write.
    let seen = Arc::new(Mutex::new(Vec::new()));
    struct TwoStalls {
        inner: TimingSink,
        writes: usize,
    }
    #[async_trait::async_trait]
    impl Sink for TwoStalls {
        async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
            self.inner.write(data).await?;
            self.writes += 1;
            if self.writes == 2 || self.writes == 7 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            Ok(())
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            Ok(())
        }
    }
    let mut sink: Box<dyn Sink> = Box::new(TwoStalls {
        inner: TimingSink {
            start: Instant::now(),
            seen: Arc::clone(&seen),
        },
        writes: 0,
    });

    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    for e in &seen {
        let slot_ms = STEP.mul_f64(e.value).as_secs_f64() * 1000.0;
        eprintln!(
            "DRIFT tick {:>2}  slot {:>6.0}ms  emitted {:>6.0}ms  drift {:>+7.0}ms",
            e.value,
            slot_ms,
            e.at.as_secs_f64() * 1000.0,
            e.at.as_secs_f64() * 1000.0 - slot_ms
        );
    }
}

/// Both silences at once: a recorded row whose own slot is in neither of them
/// must survive a stall.
///
/// The scoped re-review's B1. The frame split routes on which silence is
/// *active*, not on which construct is being judged — so the moment a periodic
/// gap is open, the whole tick lands in the wall branch, one-shot windows
/// included. Before the fix the wall branch re-derived the tick from where the
/// clock ended up, and every row owed in between was deleted:
///
/// ```text
/// played = [0, 1, 6, 7, 8, 10, 11]
///   tick 2  slot 400ms  deleted   # outside [800,1000) and outside [700,1100)
///   tick 3  slot 600ms  deleted   # outside both
/// ```
///
/// A capture replayed with a simulated outage layered over it is an ordinary
/// config, and the cross-check refuses `repeat` and `bursts:` but says nothing
/// about `gaps:`. So this is the row-frame guarantee failing from the one
/// direction validation cannot see.
///
/// Ticks 4, 5 and 9 are NOT asserted here: their slots really do sit inside one
/// of the two silences, and a row whose own slot the user covered is covered.
/// The claim is narrower than "nothing is ever suppressed" — it is that
/// falling behind is not itself a reason to drop a row.
#[tokio::test]
async fn a_stall_under_both_silences_still_owes_every_row_outside_them() {
    let config = ladder_scenario(
        "2.4s",
        Some(GapConfig {
            every: "1s".to_string(),
            r#for: "200ms".to_string(),
        }),
        Some(vec![GapWindowConfig {
            at: "700ms".to_string(),
            r#for: "400ms".to_string(),
        }]),
    );

    let (mut sink, seen) = stalling_sink(2, Duration::from_millis(600));
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    let played: Vec<u64> = seen.iter().map(|e| e.value as u64).collect();

    // Instants are deliberately not asserted: after a stall the loop is behind
    // by construction, and emitting the backlog late is the point.
    assert!(
        played.contains(&2),
        "tick 2's slot (400ms) is outside the periodic gap [800ms, 1000ms) and outside \
         the window [700ms, 1100ms), so a stall must not delete it; played {played:?}"
    );
    assert!(
        played.contains(&3),
        "tick 3's slot (600ms) is outside both silences, so a stall must not delete it; \
         played {played:?}"
    );
}

/// WALL FRAME: the same stall, and `gaps:` deliberately behaves the other way.
///
/// This is the other half of the split, and it is what makes the pair
/// discriminating: a change that moved both kinds to row frame would make the
/// row-frame test above pass and this one fail.
///
/// A recurring gap simulates an outage happening *now*. `every: 1s, for: 400ms`
/// puts the silence at `[600ms, 1000ms)` of each cycle — the same interval as
/// the row-frame case, on purpose. With the same 600ms stall, the loop reaches
/// tick 2 while real time reads ~800ms, which genuinely *is* inside the
/// simulated outage. Suppressing it is correct here: the wall says the exporter
/// is down, and a run that has fallen behind is still subject to the wall.
///
/// So tick 2 is expected to be absent — the exact opposite of the assertion
/// above, from the same stall and the same interval, differing only in which
/// key declared it.
#[tokio::test]
async fn a_stalled_sink_is_still_judged_by_the_wall_for_a_recurring_gap() {
    let config = ladder_scenario(
        "1.6s",
        Some(GapConfig {
            every: "1s".to_string(),
            r#for: "400ms".to_string(),
        }),
        None,
    );

    let (mut sink, seen) = stalling_sink(2, Duration::from_millis(600));
    runner::run_with_sink(&config, &mut sink, &CancellationToken::new(), None)
        .await
        .expect("run must succeed");

    let seen = seen.lock().expect("timing sink mutex poisoned").clone();
    let played: Vec<u64> = seen.iter().map(|e| e.value as u64).collect();

    assert!(
        !played.contains(&2),
        "a recurring gap is a wall-clock interval: the loop reached tick 2 while real \
         time was inside the outage, so it must stay suppressed; played {played:?}"
    );
}
