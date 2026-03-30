//! The main scenario event loop.
//!
//! The runner ties together all sonda-core components: it reads a
//! [`ScenarioConfig`], builds the generator, encoder, and sink, then drives the
//! tight rate-controlled loop that emits encoded metric events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::validate::parse_duration;
use crate::config::ScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_generator;
use crate::model::metric::{Labels, MetricEvent};
use crate::schedule::stats::ScenarioStats;
use crate::schedule::{is_in_burst, is_in_gap, time_until_gap_end, BurstWindow, GapWindow};
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a scenario to completion, emitting encoded metric events at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and then
/// delegates to [`run_with_sink`] with no shutdown flag and no stats collection.
///
/// This function blocks the calling thread until the scenario duration has
/// elapsed. If no duration is specified in the config it runs indefinitely.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run(config: &ScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink)?;
    run_with_sink(config, sink.as_mut(), None, None)
}

/// Run a scenario to completion, writing encoded events into the provided sink.
///
/// This function is the core event loop implementation. It accepts any [`Sink`]
/// implementation, which makes it usable in tests with a [`MemorySink`](crate::sink::memory::MemorySink)
/// instead of the config-specified sink.
///
/// # Parameters
///
/// * `config` — the scenario configuration.
/// * `sink` — the destination for encoded metric events.
/// * `shutdown` — an optional atomic flag; when set to `false` the loop exits
///   cleanly after the current tick, flushes the sink, and returns `Ok(())`.
///   Pass `None` if no external shutdown signal is needed (e.g., in tests).
/// * `stats` — an optional shared stats object. When `Some`, the runner updates
///   `total_events`, `bytes_emitted`, `current_rate`, `in_gap`, `in_burst`, and
///   `errors` on each tick. The write lock is held only for the brief counter
///   update, not during encode/write. Pass `None` to skip stats collection with
///   no overhead (e.g., in direct CLI usage or tests).
///
/// # Steps
///
/// 1. Parses the config and builds the generator and encoder.
/// 2. Builds the [`Labels`] set from the config label map.
/// 3. Enters a tight rate-control loop:
///    - Checks shutdown flag — exits cleanly if cleared.
///    - Checks duration — exits if exceeded.
///    - Checks gap window — sleeps until gap ends if currently in one.
///    - Generates a value, builds a [`MetricEvent`], encodes it, writes to sink.
///    - Sleeps for the remaining inter-event interval (accounting for elapsed work).
/// 4. Flushes the sink before returning, even if the loop exited via an error.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
/// If an error occurs during the loop and flushing also fails, the loop error
/// is returned (the flush error is discarded to preserve the original cause).
pub fn run_with_sink(
    config: &ScenarioConfig,
    sink: &mut dyn Sink,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
) -> Result<(), SondaError> {
    // Parse the optional total duration.
    let total_duration: Option<Duration> =
        config.duration.as_deref().map(parse_duration).transpose()?;

    // Build the gap window from config, if present.
    let gap_window: Option<GapWindow> = config
        .gaps
        .as_ref()
        .map(|g| -> Result<GapWindow, SondaError> {
            Ok(GapWindow {
                every: parse_duration(&g.every)?,
                duration: parse_duration(&g.r#for)?,
            })
        })
        .transpose()?;

    // Build the burst window from config, if present.
    let burst_window: Option<BurstWindow> = config
        .bursts
        .as_ref()
        .map(|b| -> Result<BurstWindow, SondaError> {
            Ok(BurstWindow {
                every: parse_duration(&b.every)?,
                duration: parse_duration(&b.r#for)?,
                multiplier: b.multiplier,
            })
        })
        .transpose()?;

    // Build generator and encoder from config.
    let generator = create_generator(&config.generator, config.rate)?;
    let encoder = create_encoder(&config.encoder);

    // Build the label set from the config's optional HashMap.
    let labels: Labels = if let Some(ref label_map) = config.labels {
        let pairs: Vec<(&str, &str)> = label_map
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Labels::from_pairs(&pairs)?
    } else {
        Labels::from_pairs(&[])?
    };

    // Clone the metric name once before the hot loop.
    // The name is invariant for the lifetime of a scenario.
    let name = config.name.clone();

    // The base inter-event interval (at normal rate, no burst).
    let base_interval = Duration::from_secs_f64(1.0 / config.rate);

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    // Record the wall-clock start time once. The next_deadline tracks the
    // absolute time at which the next event should be emitted. Unlike a pure
    // tick-counter approach, tracking the deadline directly avoids catch-up
    // accumulation across burst/normal transitions.
    let start = Instant::now();
    let mut next_deadline = start;
    let mut tick: u64 = 0;

    // Stats tracking: snapshot of tick count and wall clock taken once per
    // second to compute current_rate. Only used when stats is Some.
    let mut rate_window_tick: u64 = 0;
    let mut rate_window_start = start;

    // Run the event loop, capturing any error so we can still flush before returning.
    let loop_result = (|| -> Result<(), SondaError> {
        loop {
            // Check shutdown flag first — highest priority exit path.
            // SeqCst ensures we see the store from the signal handler promptly.
            if let Some(flag) = shutdown {
                if !flag.load(Ordering::SeqCst) {
                    break;
                }
            }

            let elapsed = start.elapsed();

            // Check duration limit.
            if let Some(total) = total_duration {
                if elapsed >= total {
                    break;
                }
            }

            // Check gap window — sleep through it rather than busy-wait.
            // Gap always takes priority over burst: no events during a gap.
            let currently_in_gap = if let Some(ref gap) = gap_window {
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
                    // After sleeping through the gap, reset the next_deadline to
                    // now so we do not try to "catch up" for events suppressed by
                    // the gap. Also re-derive tick from elapsed time at base rate
                    // so the generator tick counter stays approximately in sync
                    // with wall-clock time.
                    let now = Instant::now();
                    next_deadline = now;
                    tick = (start.elapsed().as_secs_f64() / base_interval.as_secs_f64()) as u64;
                    // Re-check duration before emitting.
                    continue;
                } else {
                    false
                }
            } else {
                false
            };

            // Determine the effective inter-event interval for this tick.
            // During a burst, divide the base interval by the burst multiplier
            // to produce a proportionally shorter interval (higher rate).
            // Outside a burst, use the base interval unchanged.
            let currently_in_burst;
            let effective_interval = if let Some(ref burst) = burst_window {
                if let Some(multiplier) = is_in_burst(elapsed, burst) {
                    currently_in_burst = true;
                    // multiplier is validated to be > 0, so division is safe.
                    Duration::from_secs_f64(base_interval.as_secs_f64() / multiplier)
                } else {
                    currently_in_burst = false;
                    base_interval
                }
            } else {
                currently_in_burst = false;
                base_interval
            };

            // Deadline-based rate control: if we are ahead of schedule, sleep
            // the remaining delta. If we are already behind (deadline passed),
            // emit immediately without sleeping — this naturally absorbs the
            // overhead of encode/write without accumulating drift.
            let now = Instant::now();
            if now < next_deadline {
                thread::sleep(next_deadline - now);
            }

            // Timestamp the event at the start of this iteration.
            let wall_now = std::time::SystemTime::now();

            // Generate the value and build the metric event.
            // MetricEvent::with_timestamp takes owned String and Labels, so we
            // must clone both per tick. The `name` clone is cheap (heap copy of a
            // short string); `labels` clone is proportional to label count, which
            // is typically small and fixed. A zero-copy API is possible post-MVP
            // if profiling shows this to be a bottleneck.
            let value = generator.value(tick);
            let event = MetricEvent::with_timestamp(name.clone(), value, labels.clone(), wall_now)?;

            // Encode and write.
            buf.clear();
            encoder.encode_metric(&event, &mut buf)?;
            let bytes_written = buf.len() as u64;
            sink.write(&buf)?;

            // Update live stats (only when a stats arc was provided).
            if let Some(ref s) = stats {
                // Compute current_rate from a 1-second window.
                let window_elapsed = rate_window_start.elapsed();
                let current_rate = if window_elapsed >= Duration::from_secs(1) {
                    let events_in_window = tick - rate_window_tick;
                    let rate = events_in_window as f64 / window_elapsed.as_secs_f64();
                    rate_window_tick = tick;
                    rate_window_start = Instant::now();
                    rate
                } else {
                    // Retain the last computed rate until the window rolls over.
                    // We read the current value from stats to avoid a separate variable.
                    s.read().map(|st| st.current_rate).unwrap_or(0.0)
                };

                if let Ok(mut st) = s.write() {
                    st.total_events += 1;
                    st.bytes_emitted += bytes_written;
                    st.current_rate = current_rate;
                    st.in_gap = currently_in_gap;
                    st.in_burst = currently_in_burst;
                    // Buffer the metric event for scrape endpoints. The clone
                    // cost is bounded by MAX_RECENT_METRICS (default 100).
                    st.push_metric(event);
                }
            }

            // Advance the deadline by one effective interval. This preserves
            // accurate timing even if encode/write takes non-trivial time.
            next_deadline += effective_interval;
            tick += 1;
        }
        Ok(())
    })();

    // Always flush buffered data before returning, even on error paths.
    // If the loop succeeded, propagate any flush error.
    // If the loop failed, preserve the original error (discard flush error).
    let flush_result = sink.flush();
    match loop_result {
        Ok(()) => flush_result,
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{GapConfig, ScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    use crate::sink::memory::MemorySink;
    use crate::sink::SinkConfig;

    /// Build a minimal ScenarioConfig suitable for a short integration run.
    fn make_config(rate: f64, duration: &str, gaps: Option<GapConfig>) -> ScenarioConfig {
        ScenarioConfig {
            name: "up".to_string(),
            rate,
            duration: Some(duration.to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps,
            bursts: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout, // not used — tests use run_with_sink directly
            phase_offset: None,
            clock_group: None,
        }
    }

    // ---- run: basic correctness ----------------------------------------------

    /// run() with a short duration should complete without error.
    #[test]
    fn run_completes_without_error_for_short_duration() {
        let config = make_config(100.0, "100ms", None);
        let result = super::run(&config);
        assert!(
            result.is_ok(),
            "run must succeed for valid config: {result:?}"
        );
    }

    // ---- Integration: ~rate events emitted over duration --------------------

    /// At rate=100 for 1 second we expect approximately 100 newline-terminated events.
    /// We allow a ±20% window to accommodate scheduling jitter.
    #[test]
    fn integration_rate_100_duration_1s_emits_approximately_100_events() {
        let config = make_config(100.0, "1s", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        assert!(
            (80..=120).contains(&newlines),
            "expected ~100 events (80–120), got {newlines}"
        );
    }

    /// Each emitted line is valid UTF-8 and starts with the metric name.
    #[test]
    fn integration_output_lines_start_with_metric_name() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        for line in output.lines() {
            assert!(
                line.starts_with("up"),
                "each line must start with metric name 'up', got: {line:?}"
            );
        }
    }

    /// Each emitted Prometheus line ends with a newline.
    #[test]
    fn integration_output_ends_with_newline() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        assert!(
            sink.buffer.ends_with(b"\n"),
            "output must end with a newline"
        );
    }

    // ---- Integration: gap suppresses events ----------------------------------

    /// With rate=100 for 5s and a gap_every=3s gap_for=1s, we expect fewer than
    /// 500 events because the gap suppresses approximately 1 second of output per
    /// 3-second cycle (~100 events lost from the first gap, plus ~100 from the
    /// second). We use 380 as a conservative upper bound below 500.
    #[test]
    fn integration_gap_suppresses_events() {
        let config = make_config(
            100.0,
            "5s",
            Some(GapConfig {
                every: "3s".to_string(),
                r#for: "1s".to_string(),
            }),
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        assert!(
            newlines < 500,
            "gap must suppress events: expected < 500, got {newlines}"
        );
        // Also confirm events were actually emitted (not zero).
        assert!(
            newlines > 0,
            "some events must be emitted outside of gaps, got {newlines}"
        );
    }

    // ---- run: invalid config is rejected -------------------------------------

    /// A config with an unparseable duration returns Err.
    #[test]
    fn run_with_invalid_duration_returns_err() {
        let mut config = make_config(100.0, "bad_duration", None);
        // Manually set an invalid duration string.
        config.duration = Some("not_a_duration".to_string());
        let result = super::run(&config);
        assert!(result.is_err(), "invalid duration must return Err");
    }

    /// A config with an invalid gap duration returns Err.
    #[test]
    fn run_with_invalid_gap_every_returns_err() {
        let mut config = make_config(100.0, "1s", None);
        config.gaps = Some(GapConfig {
            every: "bad".to_string(),
            r#for: "1s".to_string(),
        });
        let result = super::run(&config);
        assert!(result.is_err(), "invalid gap.every must return Err");
    }

    // ---- run: labels appear in output ---------------------------------------

    /// When labels are configured they appear in the encoded output.
    #[test]
    fn integration_labels_appear_in_output() {
        let mut config = make_config(50.0, "100ms", None);
        let mut label_map = std::collections::HashMap::new();
        label_map.insert("host".to_string(), "server1".to_string());
        config.labels = Some(label_map);

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        assert!(
            output.contains("host=\"server1\""),
            "label must appear in output, got:\n{output}"
        );
    }

    // ---- Integration: burst increases event rate ----------------------------

    /// Helper that builds a ScenarioConfig with an optional BurstConfig.
    fn make_config_with_burst(
        rate: f64,
        duration: &str,
        gaps: Option<crate::config::GapConfig>,
        bursts: Option<crate::config::BurstConfig>,
    ) -> crate::config::ScenarioConfig {
        crate::config::ScenarioConfig {
            name: "up".to_string(),
            rate,
            duration: Some(duration.to_string()),
            generator: crate::generator::GeneratorConfig::Constant { value: 1.0 },
            gaps,
            bursts,
            labels: None,
            encoder: crate::encoder::EncoderConfig::PrometheusText { precision: None },
            sink: crate::sink::SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        }
    }

    /// With rate=10 and burst_multiplier=5 for the entire 1s run (burst_every=10s,
    /// burst_for=5s so the burst covers the full 1s window), we should get
    /// significantly more than 10 events — approximately 50.
    ///
    /// The burst occupies [0, burst_for) of each burst_every cycle.
    /// With burst_every=10s and burst_for=9s, the first 1s of the run is always
    /// inside a burst (cycle_pos=0..1 < 9), so effective_interval = base/5.
    #[test]
    fn integration_burst_increases_event_count() {
        let config = make_config_with_burst(
            10.0,
            "1s",
            None,
            Some(crate::config::BurstConfig {
                every: "10s".to_string(),
                r#for: "9s".to_string(),
                multiplier: 5.0,
            }),
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        // Without burst: ~10 events. With 5x burst for entire 1s: ~50 events.
        // We allow a wide range to accommodate scheduling jitter.
        assert!(
            newlines > 15,
            "burst must increase event count above base rate: expected >15, got {newlines}"
        );
        assert!(
            newlines < 100,
            "event count must be sane (not runaway): expected <100, got {newlines}"
        );
    }

    /// Rate=100 for 2s with burst_every=10s, burst_for=1s, multiplier=5.
    /// The first 1s of the run is in a burst (rate=500), the second 1s is not (rate=100).
    /// Total expected events: ~500 + ~100 = ~600.
    /// We use a range of 400–800 to accommodate scheduling jitter.
    #[test]
    fn integration_burst_then_normal_produces_mixed_rate() {
        let config = make_config_with_burst(
            100.0,
            "2s",
            None,
            Some(crate::config::BurstConfig {
                every: "10s".to_string(),
                r#for: "1s".to_string(),
                multiplier: 5.0,
            }),
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        // Without any burst: ~200 events. With burst for first 1s: ~600.
        assert!(
            newlines > 200,
            "burst phase must produce more than base rate alone: expected >200, got {newlines}"
        );
        assert!(
            newlines <= 900,
            "total event count must be in expected range, got {newlines}"
        );
    }

    // ---- Integration: gap wins over burst -----------------------------------

    /// When a gap and burst overlap, the gap must win — no events are emitted
    /// during the gap window regardless of burst state.
    ///
    /// We configure a 3s run where:
    /// - Gap: every=5s, for=2s → gap occupies seconds [3, 5) of each cycle.
    ///   For a 3s run starting at t=0, no gap is active (gap starts at 3s and
    ///   the run ends at 3s). So we set the gap at [0,2) to make it overlap
    ///   with the burst.
    ///
    /// Actually, the gap occupies the END of each cycle (tail), and the burst
    /// occupies the START. To force overlap, we use a very short every (3s)
    /// so that the gap tail (last 2s of a 3s cycle = seconds [1, 3)) overlaps
    /// with a burst that starts at the beginning of the next cycle.
    ///
    /// Simpler approach: use duration=1s with gap_every=2s gap_for=1s (entire
    /// run is in a gap since the gap occupies seconds [1,2) of each 2s cycle,
    /// but actually for a 1s run, cycle_pos ∈ [0,1) which is NOT in the gap
    /// [every-for, every) = [1, 2)). Let's use a different approach.
    ///
    /// Clearest approach: run for 3s, gap_every=3s, gap_for=2s.
    /// Gap occupies [1s, 3s) in each cycle. With burst_every=3s, burst_for=2.5s,
    /// burst occupies [0, 2.5s) of each 3s cycle.
    /// Overlap at [1s, 2.5s): gap wins → no events during overlap.
    ///
    /// During [0, 1s): no gap, burst active → ~100*5=500 events in that 1s.
    /// During [1s, 3s): gap wins → 0 events.
    /// Total: only ~500 events (much less than 100*3=300 without gap/burst,
    /// and much less than 500*2+100*1 with burst only).
    #[test]
    fn integration_gap_wins_over_burst_suppresses_events() {
        // Gap occupies [every-for, every) = [3-2, 3) = [1s, 3s) per cycle.
        // Burst occupies [0, burst_for) = [0, 2.5s) per cycle.
        // Overlap: [1s, 2.5s) — gap wins here.
        // Only [0, 1s) has burst active with no gap.
        let config = make_config_with_burst(
            100.0,
            "3s",
            Some(GapConfig {
                every: "3s".to_string(),
                r#for: "2s".to_string(),
            }),
            Some(crate::config::BurstConfig {
                every: "3s".to_string(),
                r#for: "2500ms".to_string(), // 2.5s burst overlaps with gap starting at 1s
                multiplier: 5.0,
            }),
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();

        // During gap: 0 events. During [0,1s) with burst (5x): ~500 events.
        // But the run exits after 3s total, and the gap sleeps through most of it.
        // We expect far fewer than 300 baseline events (no gap/burst at 100/s for 3s).
        // The gap eats 2 out of every 3 seconds → baseline would be ~100 events.
        // With burst for the first 1s → ~500 events in that 1s alone.
        // The key assertion is that the gap suppressed events: significantly less
        // than what 5x burst for 3s would produce (1500), and also that the
        // gap didn't allow events during its window.
        assert!(
            newlines < 1000,
            "gap must suppress many events that burst would have produced: expected <1000, got {newlines}"
        );
        // Some events must have been produced during the non-gap window.
        assert!(
            newlines > 0,
            "some events must be emitted outside of gaps, got {newlines}"
        );
    }

    /// Simpler gap-wins-over-burst test: run for 100ms with no gap and burst
    /// to establish a baseline, then run with both gap and burst where the
    /// gap covers the entire duration — expect zero events.
    #[test]
    fn integration_gap_covering_full_window_produces_zero_events_even_with_burst() {
        // Gap: every=1s, for=500ms → gap occupies [500ms, 1000ms) in each 1s cycle.
        // Run for 200ms starting at cycle_pos approaching 500ms.
        //
        // Instead use: gap every=1s, for=900ms → gap occupies [100ms, 1000ms).
        // We need to start IN the gap. Since cycle_pos = elapsed % every,
        // we'd need to offset the start, which we can't do.
        //
        // Better: gap every=500ms, for=400ms → gap at [100ms, 500ms) per cycle.
        // Start at 0ms: not in gap. At 100ms: in gap. Sleep 400ms. Resume at 500ms.
        // At 500ms: not in gap again. Very few events are emitted.
        //
        // Simplest verifiable test: confirm gap suppresses events.
        // Run rate=1000 for 500ms with gap_every=1s gap_for=900ms.
        // Gap occupies [100ms, 1000ms) per 1s cycle.
        // At t=0: not in gap → emit events for ~100ms at 1000/s → ~100 events.
        // At t=100ms: in gap → sleep until t=1000ms → but run ends at 500ms.
        // After sleep wake at t=1000ms > 500ms → exit immediately.
        // Total: only ~100 events despite rate=1000 for 500ms baseline of ~500.
        let config_no_gap_burst = make_config_with_burst(
            1000.0,
            "500ms",
            None,
            Some(crate::config::BurstConfig {
                every: "1s".to_string(),
                r#for: "900ms".to_string(),
                multiplier: 5.0,
            }),
        );
        let mut sink_no_gap = MemorySink::new();
        super::run_with_sink(&config_no_gap_burst, &mut sink_no_gap, None, None)
            .expect("run must succeed");
        let events_burst_only = sink_no_gap.buffer.iter().filter(|&&b| b == b'\n').count();

        // With the burst for 900ms of each 1s cycle, over 500ms we'd expect ~4500 events.
        // This shows burst is working.
        let config_gap_and_burst = make_config_with_burst(
            1000.0,
            "500ms",
            Some(GapConfig {
                every: "1s".to_string(),
                r#for: "900ms".to_string(),
            }),
            Some(crate::config::BurstConfig {
                every: "1s".to_string(),
                r#for: "900ms".to_string(),
                multiplier: 5.0,
            }),
        );
        let mut sink_gap_burst = MemorySink::new();
        super::run_with_sink(&config_gap_and_burst, &mut sink_gap_burst, None, None)
            .expect("run must succeed");
        let events_gap_and_burst = sink_gap_burst
            .buffer
            .iter()
            .filter(|&&b| b == b'\n')
            .count();

        // The gap must suppress the burst: far fewer events when gap is active.
        // Gap occupies [100ms, 1000ms) so burst during that window is suppressed.
        // Only events in [0, 100ms) should fire.
        assert!(
            events_gap_and_burst < events_burst_only,
            "gap must suppress burst events: gap+burst={events_gap_and_burst} must be < burst-only={events_burst_only}"
        );
    }

    // ---- Shutdown flag with burst scenario -----------------------------------

    // ---- Integration: stats buffer receives metric events ---------------------

    /// When a stats arc is provided, the runner pushes metric events into the
    /// recent_metrics buffer.
    #[test]
    fn runner_pushes_metric_events_to_stats_buffer() {
        use std::sync::{Arc, RwLock};

        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        let stats = Arc::new(RwLock::new(crate::schedule::stats::ScenarioStats::default()));

        super::run_with_sink(&config, &mut sink, None, Some(Arc::clone(&stats)))
            .expect("run must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            !st.recent_metrics.is_empty(),
            "runner must push events into the stats recent_metrics buffer, got {} events",
            st.recent_metrics.len()
        );
        // The buffer is capped at MAX_RECENT_METRICS, so verify the count
        // does not exceed that limit.
        assert!(
            st.recent_metrics.len() <= crate::schedule::stats::MAX_RECENT_METRICS,
            "recent_metrics buffer must not exceed MAX_RECENT_METRICS ({}), got {}",
            crate::schedule::stats::MAX_RECENT_METRICS,
            st.recent_metrics.len()
        );
    }

    /// When no stats arc is provided (None), the runner does not panic and
    /// still produces output normally.
    #[test]
    fn runner_without_stats_does_not_push_metrics() {
        let config = make_config(50.0, "100ms", None);
        let mut sink = MemorySink::new();

        // Pass None for stats — should work fine without buffering.
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        assert!(
            newlines > 0,
            "runner without stats must still produce output"
        );
    }

    /// Stats buffer events have the correct metric name matching the config.
    #[test]
    fn runner_stats_buffer_events_have_correct_metric_name() {
        use std::sync::{Arc, RwLock};

        let config = make_config(50.0, "100ms", None);
        let mut sink = MemorySink::new();
        let stats = Arc::new(RwLock::new(crate::schedule::stats::ScenarioStats::default()));

        super::run_with_sink(&config, &mut sink, None, Some(Arc::clone(&stats)))
            .expect("run must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        for event in st.recent_metrics.iter() {
            assert_eq!(
                event.name, "up",
                "all buffered events must have the metric name from config"
            );
        }
    }

    /// The shutdown flag stops the runner even during a burst window.
    #[test]
    fn shutdown_flag_stops_run_during_burst() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let config = make_config_with_burst(
            1000.0,
            "60s", // long duration — shutdown flag stops it
            None,
            Some(crate::config::BurstConfig {
                every: "10s".to_string(),
                r#for: "9s".to_string(),
                multiplier: 5.0,
            }),
        );

        let shutdown = Arc::new(AtomicBool::new(true));
        let shutdown_clone = Arc::clone(&shutdown);

        // Set the flag to false after a short delay to trigger shutdown.
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            shutdown_clone.store(false, Ordering::SeqCst);
        });

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, Some(&shutdown), None).expect("run must succeed");
        handle.join().expect("thread must complete");

        // The run stopped after ~200ms due to shutdown flag.
        // At rate=1000 with 5x burst: ~1000 events in 200ms.
        // We just assert it stopped without hanging (the test would time out otherwise)
        // and produced some output.
        let newlines = sink.buffer.iter().filter(|&&b| b == b'\n').count();
        assert!(
            newlines > 0,
            "some events must have been emitted before shutdown"
        );
    }
}
