//! Shared schedule loop for all signal types.
//!
//! The core loop handles the schedule infrastructure that is identical across
//! metrics and logs: duration checking, shutdown handling, gap window sleeping,
//! burst window effective interval computation, deadline-based rate control,
//! spike window state tracking, and stats updating.
//!
//! Signal-specific work (event generation, encoding, sink writing) is delegated
//! to a caller-provided [`TickFn`] closure. This eliminates the duplication
//! between the metrics runner and the log runner while keeping each signal
//! type's event logic self-contained.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::metric::MetricEvent;
use crate::schedule::stats::ScenarioStats;
use crate::schedule::{is_in_burst, is_in_gap, is_in_spike, time_until_gap_end};
use crate::SondaError;

use super::ParsedSchedule;

/// The result returned by a per-tick callback.
///
/// Carries the information the shared loop needs to update stats after
/// the signal-specific work is done.
pub(crate) struct TickResult {
    /// Number of bytes written to the sink on this tick.
    pub bytes_written: u64,
    /// An optional metric event to push into the stats recent-metrics buffer.
    ///
    /// Only the metrics runner provides this; the log runner returns `None`.
    pub metric_event: Option<MetricEvent>,
}

/// Context passed to the per-tick callback.
///
/// Provides the tick index and the spike window state so the callback can
/// build the correct labels for this tick.
pub(crate) struct TickContext<'a> {
    /// The monotonically increasing tick counter (0-based).
    pub tick: u64,
    /// The resolved cardinality spike windows from the schedule config.
    ///
    /// The callback uses these along with `elapsed` to determine which spike
    /// labels to inject.
    pub spike_windows: &'a [super::CardinalitySpikeWindow],
    /// Elapsed time since the scenario started.
    ///
    /// Used by the callback to evaluate spike window state via [`is_in_spike`].
    pub elapsed: Duration,
}

/// A per-tick callback that performs signal-specific work.
///
/// Called once per scheduled tick. The callback is responsible for:
/// 1. Evaluating spike windows and building the tick's label set.
/// 2. Generating the event (metric value or log event).
/// 3. Encoding the event into the buffer.
/// 4. Writing the encoded bytes to the sink.
///
/// Returns a [`TickResult`] with the bytes written and an optional metric event
/// for stats buffering.
///
/// # Parameters
///
/// * `ctx` — context for this tick (tick index, spike windows, elapsed time).
///
/// # Errors
///
/// Returns [`SondaError`] if encoding or sink writing fails.
pub(crate) type TickFn<'a> = dyn FnMut(&TickContext<'_>) -> Result<TickResult, SondaError> + 'a;

/// Run the shared schedule loop until duration expires or shutdown is signalled.
///
/// This function owns the entire rate-control loop: shutdown detection, duration
/// checking, gap window sleeping, burst window effective interval, deadline-based
/// sleep, and stats updating. The signal-specific work (event generation,
/// encoding, sink writing) is delegated to `tick_fn`.
///
/// The caller is responsible for flushing the sink after this function returns.
/// This design avoids a double-borrow conflict: the tick closure already holds
/// `&mut sink` for per-tick writes, so the loop cannot also own it for flushing.
///
/// # Parameters
///
/// * `schedule` — the parsed schedule configuration (duration, windows).
/// * `rate` — target events per second.
/// * `shutdown` — optional atomic flag; when cleared the loop exits cleanly.
/// * `stats` — optional shared stats for live telemetry.
/// * `tick_fn` — per-tick callback for signal-specific work.
///
/// # Errors
///
/// Returns [`SondaError`] if the tick callback fails.
pub(crate) fn run_schedule_loop(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    tick_fn: &mut TickFn<'_>,
) -> Result<(), SondaError> {
    let base_interval = Duration::from_secs_f64(1.0 / rate);

    let start = Instant::now();
    let mut next_deadline = start;
    let mut tick: u64 = 0;

    // Stats tracking: snapshot of tick count and wall clock taken once per
    // second to compute current_rate.
    let mut rate_window_tick: u64 = 0;
    let mut rate_window_start = start;

    loop {
        // Check shutdown flag first — highest priority exit path.
        if let Some(flag) = shutdown {
            if !flag.load(Ordering::SeqCst) {
                break;
            }
        }

        let elapsed = start.elapsed();

        // Check duration limit.
        if let Some(total) = schedule.total_duration {
            if elapsed >= total {
                break;
            }
        }

        // Check gap window — sleep through it rather than busy-wait.
        // Gap always takes priority over burst: no events during a gap.
        if let Some(ref gap) = schedule.gap_window {
            if is_in_gap(elapsed, gap) {
                // Update stats to reflect gap state before sleeping.
                if let Some(ref s) = stats {
                    if let Ok(mut st) = s.write() {
                        st.in_gap = true;
                        st.in_burst = false;
                    }
                }
                let sleep_for = time_until_gap_end(elapsed, gap);
                if sleep_for > Duration::ZERO {
                    thread::sleep(sleep_for);
                }
                // After sleeping through the gap, reset the deadline so we
                // don't try to catch up for suppressed events. Re-derive
                // tick from elapsed time at base rate.
                let now = Instant::now();
                next_deadline = now;
                tick = (start.elapsed().as_secs_f64() / base_interval.as_secs_f64()) as u64;
                continue;
            }
        }

        // We are not in a gap — `currently_in_gap` is always false here because
        // the gap branch above continues the loop instead of falling through.
        let currently_in_gap = false;

        // Determine the effective inter-event interval for this tick.
        let currently_in_burst;
        let effective_interval = if let Some(ref burst) = schedule.burst_window {
            if let Some(multiplier) = is_in_burst(elapsed, burst) {
                currently_in_burst = true;
                Duration::from_secs_f64(base_interval.as_secs_f64() / multiplier)
            } else {
                currently_in_burst = false;
                base_interval
            }
        } else {
            currently_in_burst = false;
            base_interval
        };

        // Deadline-based rate control.
        let now = Instant::now();
        if now < next_deadline {
            thread::sleep(next_deadline - now);
        }

        // Invoke the signal-specific tick callback.
        let ctx = TickContext {
            tick,
            spike_windows: &schedule.spike_windows,
            elapsed,
        };
        let result = tick_fn(&ctx)?;

        // Determine spike state for stats (check all spike windows).
        let currently_in_spike = schedule
            .spike_windows
            .iter()
            .any(|sw| is_in_spike(elapsed, sw));

        // Update live stats (only when a stats arc was provided).
        if let Some(ref s) = stats {
            let window_elapsed = rate_window_start.elapsed();
            let current_rate = if window_elapsed >= Duration::from_secs(1) {
                let events_in_window = tick - rate_window_tick;
                let r = events_in_window as f64 / window_elapsed.as_secs_f64();
                rate_window_tick = tick;
                rate_window_start = Instant::now();
                r
            } else {
                s.read().map(|st| st.current_rate).unwrap_or(0.0)
            };

            if let Ok(mut st) = s.write() {
                st.total_events += 1;
                st.bytes_emitted += result.bytes_written;
                st.current_rate = current_rate;
                st.in_gap = currently_in_gap;
                st.in_burst = currently_in_burst;
                st.in_cardinality_spike = currently_in_spike;
                if let Some(event) = result.metric_event {
                    st.push_metric(event);
                }
            }
        }

        next_deadline += effective_interval;
        tick += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{BurstWindow, GapWindow};

    /// Build a minimal ParsedSchedule for testing.
    fn minimal_schedule(duration: Option<Duration>) -> ParsedSchedule {
        ParsedSchedule {
            total_duration: duration,
            gap_window: None,
            burst_window: None,
            spike_windows: Vec::new(),
        }
    }

    // ---- Basic loop: runs for duration, emits events -------------------------

    /// The loop emits events at the configured rate for the configured duration.
    #[test]
    fn loop_emits_events_for_duration() {
        let schedule = minimal_schedule(Some(Duration::from_millis(500)));

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 6,
                metric_event: None,
            })
        };

        run_schedule_loop(
            &schedule,
            20.0, // 20 events/sec for 500ms = ~10 events
            None,
            None,
            &mut tick_fn,
        )
        .expect("loop must succeed");

        assert!(
            event_count > 5,
            "expected ~10 events at 20/s for 500ms, got {event_count}"
        );
        assert!(
            event_count < 20,
            "expected ~10 events, got {event_count} (too many)"
        );
    }

    // ---- Shutdown flag: stops the loop early --------------------------------

    /// Clearing the shutdown flag stops the loop before duration expires.
    #[test]
    fn loop_stops_on_shutdown_flag() {
        use std::sync::atomic::AtomicBool;

        let schedule = minimal_schedule(None); // indefinite
        let mut event_count: u64 = 0;

        // Spawn a thread to clear the flag after 200ms.
        let shutdown_arc = Arc::new(AtomicBool::new(true));
        let flag_clone = Arc::clone(&shutdown_arc);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag_clone.store(false, Ordering::SeqCst);
        });

        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                metric_event: None,
            })
        };

        run_schedule_loop(
            &schedule,
            50.0,
            Some(shutdown_arc.as_ref()),
            None,
            &mut tick_fn,
        )
        .expect("loop must succeed");

        handle.join().expect("thread must complete");

        assert!(
            event_count > 0,
            "some events should have been emitted before shutdown"
        );
    }

    // ---- Gap window: suppresses events during gap ---------------------------

    /// Events are suppressed during a gap window.
    #[test]
    fn loop_suppresses_events_during_gap() {
        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_secs(2)),
            gap_window: Some(GapWindow {
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9), // gap from 1s to 10s
            }),
            burst_window: None,
            spike_windows: Vec::new(),
        };

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                metric_event: None,
            })
        };

        run_schedule_loop(&schedule, 100.0, None, None, &mut tick_fn).expect("loop must succeed");

        // Only ~100 events from the first 1s before the gap kicks in.
        assert!(
            event_count < 150,
            "gap should suppress events: expected < 150, got {event_count}"
        );
    }

    // ---- Burst window: increases event rate ---------------------------------

    /// Burst window increases the effective rate.
    #[test]
    fn loop_increases_rate_during_burst() {
        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_secs(1)),
            gap_window: None,
            burst_window: Some(BurstWindow {
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9), // burst covers full 1s run
                multiplier: 5.0,
            }),
            spike_windows: Vec::new(),
        };

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                metric_event: None,
            })
        };

        run_schedule_loop(&schedule, 10.0, None, None, &mut tick_fn).expect("loop must succeed");

        // Without burst: ~10 events. With 5x burst: ~50 events.
        assert!(
            event_count > 15,
            "burst should increase event count: expected >15, got {event_count}"
        );
    }

    // ---- Stats tracking: updates stats arc ----------------------------------

    /// Stats are updated correctly when a stats arc is provided.
    #[test]
    fn loop_updates_stats() {
        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            Ok(TickResult {
                bytes_written: 42,
                metric_event: None,
            })
        };

        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut tick_fn,
        )
        .expect("loop must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            st.total_events > 0,
            "stats must track total_events, got {}",
            st.total_events
        );
        assert!(
            st.bytes_emitted > 0,
            "stats must track bytes_emitted, got {}",
            st.bytes_emitted
        );
    }

    // ---- Stats tracking: metric events pushed to buffer ---------------------

    /// When the tick callback returns a MetricEvent, it is pushed to the stats buffer.
    #[test]
    fn loop_pushes_metric_events_to_stats_buffer() {
        use crate::model::metric::{Labels, MetricEvent};

        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            let event = MetricEvent::new("test".to_string(), 1.0, Labels::default())
                .expect("valid metric name");
            Ok(TickResult {
                bytes_written: 10,
                metric_event: Some(event),
            })
        };

        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut tick_fn,
        )
        .expect("loop must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            !st.recent_metrics.is_empty(),
            "stats buffer must contain metric events"
        );
    }

    // ---- Tick context: spike windows are passed to callback -----------------

    /// The tick callback receives spike windows in the context.
    #[test]
    fn loop_passes_spike_windows_to_tick_fn() {
        use crate::config::SpikeStrategy;
        use crate::schedule::CardinalitySpikeWindow;

        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_millis(100)),
            gap_window: None,
            burst_window: None,
            spike_windows: vec![CardinalitySpikeWindow {
                label: "pod".to_string(),
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9),
                cardinality: 5,
                strategy: SpikeStrategy::Counter,
                prefix: "pod-".to_string(),
                seed: 0,
            }],
        };

        let mut saw_spike_windows = false;
        let mut tick_fn = |ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            if !ctx.spike_windows.is_empty() {
                saw_spike_windows = true;
            }
            Ok(TickResult {
                bytes_written: 0,
                metric_event: None,
            })
        };

        run_schedule_loop(&schedule, 100.0, None, None, &mut tick_fn).expect("loop must succeed");

        assert!(
            saw_spike_windows,
            "tick callback must receive spike windows"
        );
    }

    // ---- Error propagation: tick errors are propagated ----------------------

    /// When the tick callback returns an error, the loop propagates it.
    #[test]
    fn loop_propagates_tick_error() {
        let schedule = minimal_schedule(Some(Duration::from_secs(10)));

        let mut tick_fn = |_ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
            Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::Other,
                "test error",
            )))
        };

        let result = run_schedule_loop(&schedule, 10.0, None, None, &mut tick_fn);

        assert!(result.is_err(), "loop must propagate tick error");
    }

    // ---- Contract: TickResult fields ----------------------------------------

    /// TickResult correctly carries bytes_written and metric_event.
    #[test]
    fn tick_result_carries_all_fields() {
        use crate::model::metric::{Labels, MetricEvent};

        let event =
            MetricEvent::new("test".to_string(), 42.0, Labels::default()).expect("valid name");
        let result = TickResult {
            bytes_written: 100,
            metric_event: Some(event),
        };

        assert_eq!(result.bytes_written, 100);
        assert!(result.metric_event.is_some());
    }
}
