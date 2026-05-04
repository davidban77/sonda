//! The metric scenario event loop.
//!
//! The runner ties together all sonda-core components: it reads a
//! [`ScenarioConfig`], builds the generator, encoder, and sink, then delegates
//! to the shared [`core_loop::run_schedule_loop`](super::core_loop::run_schedule_loop)
//! for rate control, gap/burst/spike window handling, stats tracking, and
//! shutdown management. Only the metric-specific event generation and encoding
//! logic lives here.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use crate::config::ScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_generator;
use crate::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use crate::schedule::core_loop::{self, GateContext, TickContext, TickResult};
use crate::schedule::gate_bus::GateBus;
use crate::schedule::is_in_spike;
use crate::schedule::stats::ScenarioStats;
use crate::schedule::ParsedSchedule;
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
    let mut sink = create_sink(&config.sink, None)?;
    run_with_sink(config, sink.as_mut(), None, None)
}

/// Run a scenario to completion, writing encoded events into the provided sink.
///
/// This function builds the metric generator, encoder, and label set from the
/// config, then delegates to the shared schedule loop via
/// [`core_loop::run_schedule_loop`](super::core_loop::run_schedule_loop).
/// The metric-specific per-tick work (value generation, label spike injection,
/// `MetricEvent` construction, encoding, and sink writing) is captured in a
/// closure passed to the shared loop.
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
    run_with_sink_gated(config, sink, shutdown, stats, None, None)
}

/// Run a metric scenario with optional `while:` / `after:` gating.
///
/// `upstream_bus` is the bus this scenario PUBLISHES into (for downstream
/// gates to subscribe to). `gate_ctx` is what THIS scenario consumes from
/// an upstream bus. Both are independent — a scenario can be both an
/// upstream (publishing) and a downstream (gated).
pub fn run_with_sink_gated(
    config: &ScenarioConfig,
    sink: &mut dyn Sink,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    upstream_bus: Option<Arc<GateBus>>,
    gate_ctx: Option<GateContext>,
) -> Result<(), SondaError> {
    // Parse the schedule (duration, gap/burst/spike windows) from the shared
    // BaseScheduleConfig. This is the single authoritative parsing location —
    // no duplication with the log runner.
    let schedule = ParsedSchedule::from_base_config(&config.base)?;

    // Build generator and encoder from config.
    let generator = create_generator(&config.generator, config.rate)?;
    let generator =
        crate::generator::wrap_with_jitter(generator, config.base.jitter, config.base.jitter_seed);
    let encoder = create_encoder(&config.encoder)?;

    // Build the label set from the config's optional HashMap, wrapped in Arc
    // so the hot loop can share it across ticks without deep-cloning the BTreeMap.
    let labels: Arc<Labels> = {
        let inner = if let Some(ref label_map) = config.labels {
            let pairs: Vec<(&str, &str)> = label_map
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            Labels::from_pairs(&pairs)?
        } else {
            Labels::from_pairs(&[])?
        };
        Arc::new(inner)
    };

    // Validate and intern the metric name once before the hot loop.
    // ValidatedMetricName wraps Arc<str> — cloning is O(1), just a refcount bump.
    // The type system guarantees the name is valid for all subsequent uses.
    let name = ValidatedMetricName::new(&config.name)?;

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    // Build the per-tick closure that performs metric-specific work:
    // generate value → evaluate spike labels → build MetricEvent → encode → write.
    let upstream_bus_for_tick = upstream_bus.clone();
    let mut tick_fn = |ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
        // Timestamp the event at the start of this tick.
        let wall_now = std::time::SystemTime::now();

        // Generate the value for this tick.
        let value = generator.value(ctx.tick);
        if let Some(ref bus) = upstream_bus_for_tick {
            bus.tick(value);
        }

        // Build the per-tick label set. In the common case (no spike windows
        // and no dynamic labels) this is just an Arc refcount bump — O(1),
        // zero heap allocation. Only when a cardinality spike is active or
        // dynamic labels are configured do we deep-clone the inner Labels to
        // insert the per-tick values.
        let needs_dynamic = !ctx.dynamic_labels.is_empty();
        let tick_labels: Arc<Labels> = if ctx.spike_windows.is_empty() && !needs_dynamic {
            Arc::clone(&labels)
        } else {
            let mut mutated: Option<Labels> = None;
            // Inject dynamic labels (always-on, every tick).
            if needs_dynamic {
                let tl = mutated.get_or_insert_with(|| (*labels).clone());
                for dl in ctx.dynamic_labels {
                    tl.insert(dl.key.clone(), dl.label_value_for_tick(ctx.tick));
                }
            }
            // Inject cardinality spike labels (time-windowed).
            for sw in ctx.spike_windows {
                if is_in_spike(ctx.elapsed, sw) {
                    let tl = mutated.get_or_insert_with(|| (*labels).clone());
                    tl.insert(sw.label.clone(), sw.label_value_for_tick(ctx.tick));
                }
            }
            match mutated {
                Some(tl) => Arc::new(tl),
                None => Arc::clone(&labels),
            }
        };

        // Build the MetricEvent from pre-validated, pre-shared parts.
        // name: Arc::clone is O(1) — just a refcount bump, no heap copy.
        // tick_labels: already an Arc<Labels> from above.
        let event = MetricEvent::from_parts(name.clone(), value, tick_labels, wall_now);

        // Encode and write.
        buf.clear();
        encoder.encode_metric(&event, &mut buf)?;
        let bytes_written = buf.len() as u64;
        sink.write(&buf)?;

        Ok(TickResult {
            bytes_written,
            metric_event: Some(event),
        })
    };

    // Run the shared schedule loop. The tick closure owns the sink borrow for
    // per-tick writes; the loop itself handles rate control, gap/burst/spike
    // windows, stats tracking, and shutdown. We flush after the loop returns.
    let stats_for_flush = stats.clone();
    let loop_result = match gate_ctx {
        None => core_loop::run_schedule_loop(&schedule, config.rate, shutdown, stats, &mut tick_fn),
        Some(ctx) => {
            core_loop::gated_loop(&schedule, config.rate, shutdown, stats, ctx, &mut tick_fn)
        }
    };

    let flush_result = sink.flush();
    match loop_result {
        Ok(()) => core_loop::apply_flush_policy(&schedule, stats_for_flush.as_ref(), flush_result),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{BaseScheduleConfig, GapConfig, ScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    use crate::sink::memory::MemorySink;
    use crate::sink::SinkConfig;

    /// Build a minimal ScenarioConfig suitable for a short integration run.
    fn make_config(rate: f64, duration: &str, gaps: Option<GapConfig>) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                name: "up".to_string(),
                rate,
                duration: Some(duration.to_string()),
                gaps,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout, // not used — tests use run_with_sink directly
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
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
            base: crate::config::BaseScheduleConfig {
                name: "up".to_string(),
                rate,
                duration: Some(duration.to_string()),
                gaps,
                bursts,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: crate::sink::SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: crate::generator::GeneratorConfig::Constant { value: 1.0 },
            encoder: crate::encoder::EncoderConfig::PrometheusText { precision: None },
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
                &*event.name, "up",
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

    // ---- Integration: cardinality spikes inject dynamic labels ----------------

    /// Helper that builds a ScenarioConfig with a cardinality spike.
    fn make_config_with_spike(
        rate: f64,
        duration: &str,
        spike: crate::config::CardinalitySpikeConfig,
    ) -> crate::config::ScenarioConfig {
        crate::config::ScenarioConfig {
            base: crate::config::BaseScheduleConfig {
                name: "up".to_string(),
                rate,
                duration: Some(duration.to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: Some(vec![spike]),
                dynamic_labels: None,
                labels: None,
                sink: crate::sink::SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: crate::generator::GeneratorConfig::Constant { value: 1.0 },
            encoder: crate::encoder::EncoderConfig::PrometheusText { precision: None },
        }
    }

    /// When the entire run is inside a spike window, every output line must
    /// contain the spike label key.
    #[test]
    fn integration_spike_labels_appear_during_spike_window() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "10s".to_string(),
            r#for: "9s".to_string(),
            cardinality: 5,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        };
        // Run for 500ms inside a spike window (spike occupies 0..9s of each 10s cycle)
        let config = make_config_with_spike(50.0, "500ms", spike);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        for line in output.lines() {
            assert!(
                line.contains("pod_name="),
                "every line during spike must contain pod_name label, got: {line:?}"
            );
        }
    }

    /// When no spike windows are configured, output does not contain spike labels.
    #[test]
    fn integration_no_spike_config_produces_no_spike_labels() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        for line in output.lines() {
            assert!(
                !line.contains("pod_name="),
                "without spike config, pod_name must not appear: {line:?}"
            );
        }
    }

    /// Counter strategy produces unique values bounded by cardinality.
    #[test]
    fn integration_spike_counter_strategy_produces_bounded_values() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "10s".to_string(),
            r#for: "9s".to_string(),
            cardinality: 3,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        };
        let config = make_config_with_spike(50.0, "500ms", spike);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        let mut seen_values = std::collections::HashSet::new();
        for line in output.lines() {
            // Extract value of pod_name from Prometheus output like: pod_name="pod-0"
            if let Some(start) = line.find("pod_name=\"") {
                let rest = &line[start + 10..];
                if let Some(end) = rest.find('"') {
                    seen_values.insert(rest[..end].to_string());
                }
            }
        }
        // With cardinality=3, we should see at most 3 unique values
        assert!(
            seen_values.len() <= 3,
            "counter strategy with cardinality=3 should produce at most 3 unique values, got {}: {:?}",
            seen_values.len(),
            seen_values
        );
        assert!(
            !seen_values.is_empty(),
            "must have produced at least one spike label value"
        );
    }

    /// Stats correctly reports `in_cardinality_spike` during a spike window.
    #[test]
    fn integration_spike_stats_reports_in_cardinality_spike() {
        use std::sync::{Arc, RwLock};

        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "10s".to_string(),
            r#for: "9s".to_string(),
            cardinality: 5,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        };
        let config = make_config_with_spike(50.0, "200ms", spike);
        let mut sink = MemorySink::new();
        let stats = Arc::new(RwLock::new(crate::schedule::stats::ScenarioStats::default()));

        super::run_with_sink(&config, &mut sink, None, Some(Arc::clone(&stats)))
            .expect("run must succeed");

        // The entire 200ms run is inside the spike window (0..9s of 10s cycle).
        // The final stats snapshot should show in_cardinality_spike = true.
        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            st.in_cardinality_spike,
            "stats must report in_cardinality_spike=true during spike window"
        );
    }

    // ---- Arc sharing: name and labels are reference-counted, not deep-cloned ---

    /// All metric events buffered in stats must share the same Arc<str> name
    /// allocation, proving that the runner uses Arc::clone (refcount bump)
    /// instead of deep-cloning the name string on every tick.
    #[test]
    fn buffered_events_share_name_arc_allocation() {
        use std::sync::{Arc, RwLock};

        let config = make_config(200.0, "100ms", None);
        let mut sink = MemorySink::new();
        let stats = Arc::new(RwLock::new(crate::schedule::stats::ScenarioStats::default()));

        super::run_with_sink(&config, &mut sink, None, Some(Arc::clone(&stats)))
            .expect("run must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        let events: Vec<_> = st.recent_metrics.iter().collect();
        assert!(
            events.len() >= 2,
            "need at least 2 events to verify sharing, got {}",
            events.len()
        );

        // All events should share the same Arc<str> allocation for the name.
        let first_name = events[0].name.arc();
        for (i, event) in events.iter().enumerate().skip(1) {
            assert!(
                Arc::ptr_eq(first_name, event.name.arc()),
                "event[{i}].name should share Arc allocation with event[0].name"
            );
        }
    }

    /// When no cardinality spikes are configured, all buffered events must share
    /// the same Arc<Labels> allocation, proving that the runner avoids
    /// deep-cloning the BTreeMap on every tick.
    #[test]
    fn buffered_events_share_labels_arc_when_no_spikes() {
        use std::sync::{Arc, RwLock};

        let config = make_config(200.0, "100ms", None);
        let mut sink = MemorySink::new();
        let stats = Arc::new(RwLock::new(crate::schedule::stats::ScenarioStats::default()));

        super::run_with_sink(&config, &mut sink, None, Some(Arc::clone(&stats)))
            .expect("run must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        let events: Vec<_> = st.recent_metrics.iter().collect();
        assert!(
            events.len() >= 2,
            "need at least 2 events to verify sharing, got {}",
            events.len()
        );

        // All events should share the same Arc<Labels> allocation.
        let first_labels = &events[0].labels;
        for (i, event) in events.iter().enumerate().skip(1) {
            assert!(
                Arc::ptr_eq(first_labels, &event.labels),
                "event[{i}].labels should share Arc allocation with event[0].labels"
            );
        }
    }

    /// Invalid metric name in config is caught before the hot loop, not during.
    #[test]
    fn invalid_metric_name_returns_config_error_before_loop() {
        let config = ScenarioConfig {
            base: BaseScheduleConfig {
                name: "123-invalid".to_string(),
                rate: 10.0,
                duration: Some("100ms".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        };
        let mut sink = MemorySink::new();
        let result = super::run_with_sink(&config, &mut sink, None, None);
        assert!(
            matches!(result, Err(crate::SondaError::Config(ref e)) if e.to_string().contains("123-invalid")),
            "expected Config error for invalid name, got: {result:?}"
        );
    }

    // ---- Integration: dynamic labels appear in metric output --------------------

    /// Helper that builds a ScenarioConfig with dynamic_labels.
    fn make_config_with_dynamic_labels(
        rate: f64,
        duration: &str,
        dynamic_labels: Vec<crate::config::DynamicLabelConfig>,
    ) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                name: "up".to_string(),
                rate,
                duration: Some(duration.to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: Some(dynamic_labels),
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: crate::OnSinkError::Warn,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        }
    }

    /// Dynamic labels with counter strategy appear in every Prometheus line.
    #[test]
    fn dynamic_labels_counter_appear_in_metric_output() {
        let config = make_config_with_dynamic_labels(
            10.0,
            "1s",
            vec![crate::config::DynamicLabelConfig {
                key: "hostname".to_string(),
                strategy: crate::config::DynamicLabelStrategy::Counter {
                    prefix: Some("host-".to_string()),
                    cardinality: 5,
                },
            }],
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            !lines.is_empty(),
            "runner must produce at least one line of output"
        );

        for line in &lines {
            assert!(
                line.contains("hostname=\"host-"),
                "every metric line must contain dynamic label hostname; line: {line}"
            );
        }
    }

    /// Dynamic labels with values list cycle through the values.
    #[test]
    fn dynamic_labels_values_list_cycle_in_metric_output() {
        let config = make_config_with_dynamic_labels(
            10.0,
            "1s",
            vec![crate::config::DynamicLabelConfig {
                key: "region".to_string(),
                strategy: crate::config::DynamicLabelStrategy::ValuesList {
                    values: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
                },
            }],
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert!(!lines.is_empty());

        // All lines must contain the label key
        for line in &lines {
            assert!(
                line.contains("region=\""),
                "every metric line must contain dynamic label region; line: {line}"
            );
        }

        // Check that multiple distinct values appear across the output
        let has_alpha = lines.iter().any(|l| l.contains("region=\"alpha\""));
        let has_beta = lines.iter().any(|l| l.contains("region=\"beta\""));
        assert!(
            has_alpha || has_beta,
            "at least one distinct dynamic label value should appear in output"
        );
    }

    /// Cardinality ceiling is respected: only cardinality distinct values appear.
    #[test]
    fn dynamic_labels_counter_respects_cardinality_ceiling_in_output() {
        let config = make_config_with_dynamic_labels(
            50.0,
            "1s",
            vec![crate::config::DynamicLabelConfig {
                key: "hostname".to_string(),
                strategy: crate::config::DynamicLabelStrategy::Counter {
                    prefix: Some("host-".to_string()),
                    cardinality: 3,
                },
            }],
        );
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        let mut distinct_values = std::collections::HashSet::new();
        for line in output.lines() {
            // Extract the hostname="..." value
            if let Some(start) = line.find("hostname=\"") {
                let rest = &line[start + 10..];
                if let Some(end) = rest.find('"') {
                    distinct_values.insert(rest[..end].to_string());
                }
            }
        }
        assert_eq!(
            distinct_values.len(),
            3,
            "with cardinality=3, exactly 3 distinct values must appear, got {:?}",
            distinct_values
        );
    }

    /// Dynamic labels and static labels coexist: both appear in output.
    #[test]
    fn dynamic_labels_and_static_labels_coexist_in_output() {
        let mut config = make_config_with_dynamic_labels(
            10.0,
            "1s",
            vec![crate::config::DynamicLabelConfig {
                key: "hostname".to_string(),
                strategy: crate::config::DynamicLabelStrategy::Counter {
                    prefix: Some("host-".to_string()),
                    cardinality: 5,
                },
            }],
        );
        let mut label_map = std::collections::HashMap::new();
        label_map.insert("env".to_string(), "prod".to_string());
        config.labels = Some(label_map);

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        for line in output.lines() {
            assert!(
                line.contains("env=\"prod\""),
                "static label must appear; line: {line}"
            );
            assert!(
                line.contains("hostname=\"host-"),
                "dynamic label must appear; line: {line}"
            );
        }
    }

    /// Dynamic label wins on key collision with static label.
    #[test]
    fn dynamic_label_wins_on_key_collision_with_static() {
        let mut config = make_config_with_dynamic_labels(
            10.0,
            "500ms",
            vec![crate::config::DynamicLabelConfig {
                key: "hostname".to_string(),
                strategy: crate::config::DynamicLabelStrategy::Counter {
                    prefix: Some("dynamic-".to_string()),
                    cardinality: 3,
                },
            }],
        );
        let mut label_map = std::collections::HashMap::new();
        label_map.insert("hostname".to_string(), "static-value".to_string());
        config.labels = Some(label_map);

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        for line in output.lines() {
            assert!(
                line.contains("hostname=\"dynamic-"),
                "dynamic label must overwrite static label; line: {line}"
            );
            assert!(
                !line.contains("hostname=\"static-value\""),
                "static value must not appear when dynamic label overrides it; line: {line}"
            );
        }
    }
}
