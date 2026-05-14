//! The histogram scenario event loop.
//!
//! The histogram runner ties together the [`HistogramGenerator`], encoder, and
//! sink with the shared schedule loop from
//! [`core_loop::run_schedule_loop`](super::core_loop::run_schedule_loop).
//!
//! Each tick, the runner:
//! 1. Advances the histogram generator to get a [`HistogramSample`].
//! 2. For each bucket boundary, creates a `MetricEvent` with name
//!    `{base}_bucket` and an `le="{bound}"` label.
//! 3. Creates a `+Inf` bucket event (`le="+Inf"`, value = total count).
//! 4. Creates `{base}_count` and `{base}_sum` events.
//! 5. Encodes all events and writes them to the sink.
//!
//! The core loop is unchanged — all histogram-specific logic lives in the
//! per-tick closure.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use crate::config::HistogramScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::histogram::{to_distribution, HistogramGenerator, DEFAULT_HISTOGRAM_BUCKETS};
use crate::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use crate::schedule::core_loop::{self, GateContext, TickContext, TickResult};
use crate::schedule::is_in_spike;
use crate::schedule::stats::ScenarioStats;
use crate::schedule::ParsedSchedule;
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a histogram scenario to completion, emitting encoded histogram events
/// at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and
/// delegates to [`run_with_sink`] with no shutdown flag and no stats collection.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run(config: &HistogramScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink, None)?;
    run_with_sink(config, sink.as_mut(), None, None)
}

/// Run a histogram scenario to completion, writing encoded events into the
/// provided sink.
///
/// Builds the histogram generator, encoder, and label sets from the config,
/// then delegates to the shared schedule loop. The per-tick closure generates
/// multiple `MetricEvent`s per tick (one per bucket + `+Inf` + `_count` + `_sum`).
///
/// # Parameters
///
/// * `config` — the histogram scenario configuration.
/// * `sink` — the destination for encoded metric events.
/// * `shutdown` — optional atomic flag for clean shutdown.
/// * `stats` — optional shared stats for live telemetry.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run_with_sink(
    config: &HistogramScenarioConfig,
    sink: &mut dyn Sink,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
) -> Result<(), SondaError> {
    run_with_sink_gated(config, sink, shutdown, stats, None)
}

/// Run a histogram scenario with optional `while:` / `after:` gating.
///
/// Histograms cannot be `while:` upstreams (compile-time
/// `NonMetricsTarget`), but they can be `while:`-gated downstreams.
pub fn run_with_sink_gated(
    config: &HistogramScenarioConfig,
    sink: &mut dyn Sink,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    gate_ctx: Option<GateContext>,
) -> Result<(), SondaError> {
    let schedule = ParsedSchedule::from_base_config(&config.base)?;

    // Resolve histogram parameters with defaults.
    let buckets: Vec<f64> = config
        .buckets
        .clone()
        .unwrap_or_else(|| DEFAULT_HISTOGRAM_BUCKETS.to_vec());
    let distribution = to_distribution(&config.distribution);
    let observations_per_tick = config.observations_per_tick.unwrap_or(100);
    let mean_shift_per_sec = config.mean_shift_per_sec.unwrap_or(0.0);
    let seed = config.seed.unwrap_or(0);

    let mut histogram_gen = HistogramGenerator::new(
        buckets.clone(),
        distribution,
        observations_per_tick,
        mean_shift_per_sec,
        seed,
        config.rate,
    );

    let encoder = create_encoder(&config.encoder)?;

    // Build the base label set from config.
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

    // Pre-validate and intern metric names once before the hot loop.
    let bucket_name = ValidatedMetricName::new(&format!("{}_bucket", config.name))?;
    let count_name = ValidatedMetricName::new(&format!("{}_count", config.name))?;
    let sum_name = ValidatedMetricName::new(&format!("{}_sum", config.name))?;

    // Pre-build `le` label strings for each bucket boundary.
    let le_strings: Vec<String> = buckets.iter().map(|b| format_le_value(*b)).collect();

    // Pre-build Arc<Labels> for each bucket (base labels + le="{bound}"), +Inf,
    // _count, and _sum before the hot loop. In steady state (no spikes or
    // dynamic labels), these are reused via Arc::clone — zero heap allocations
    // per tick.
    let prebuilt_bucket_labels: Vec<Arc<Labels>> = le_strings
        .iter()
        .map(|le_val| {
            let mut bl = (*labels).clone();
            bl.insert("le".to_string(), le_val.clone());
            Arc::new(bl)
        })
        .collect();
    let prebuilt_inf_labels: Arc<Labels> = {
        let mut bl = (*labels).clone();
        bl.insert("le".to_string(), "+Inf".to_string());
        Arc::new(bl)
    };
    // _count and _sum share the base labels (no `le`).
    let prebuilt_count_sum_labels: Arc<Labels> = Arc::clone(&labels);

    // Pre-allocate encode buffer.
    let mut buf: Vec<u8> = Vec::with_capacity(1024);

    let mut tick_fn =
        |ctx: &TickContext<'_>, sink: &mut dyn Sink| -> Result<TickResult, SondaError> {
            let wall_now = std::time::SystemTime::now();

            // Advance the histogram generator.
            let sample = histogram_gen.observe(ctx.tick);

            // Determine whether dynamic labels or spikes are active this tick.
            let needs_dynamic = !ctx.dynamic_labels.is_empty();
            let has_active_spike = ctx
                .spike_windows
                .iter()
                .any(|sw| is_in_spike(ctx.elapsed, sw));
            let needs_clone = needs_dynamic || has_active_spike;

            let mut total_bytes: u64 = 0;

            // Emit one event per bucket boundary.
            for (i, &bucket_count) in sample.bucket_counts.iter().enumerate() {
                let bucket_labels = if needs_clone {
                    let mut bl = (*prebuilt_bucket_labels[i]).clone();
                    for dl in ctx.dynamic_labels {
                        bl.insert(dl.key.clone(), dl.label_value_for_tick(ctx.tick));
                    }
                    for sw in ctx.spike_windows {
                        if is_in_spike(ctx.elapsed, sw) {
                            bl.insert(sw.label.clone(), sw.label_value_for_tick(ctx.tick));
                        }
                    }
                    Arc::new(bl)
                } else {
                    Arc::clone(&prebuilt_bucket_labels[i])
                };
                let event = MetricEvent::from_parts(
                    bucket_name.clone(),
                    bucket_count as f64,
                    bucket_labels,
                    wall_now,
                );
                buf.clear();
                encoder.encode_metric(&event, &mut buf)?;
                total_bytes += buf.len() as u64;
                sink.write(&buf)?;
            }

            // Emit +Inf bucket (value = total count).
            {
                let inf_labels = if needs_clone {
                    let mut bl = (*prebuilt_inf_labels).clone();
                    for dl in ctx.dynamic_labels {
                        bl.insert(dl.key.clone(), dl.label_value_for_tick(ctx.tick));
                    }
                    for sw in ctx.spike_windows {
                        if is_in_spike(ctx.elapsed, sw) {
                            bl.insert(sw.label.clone(), sw.label_value_for_tick(ctx.tick));
                        }
                    }
                    Arc::new(bl)
                } else {
                    Arc::clone(&prebuilt_inf_labels)
                };
                let event = MetricEvent::from_parts(
                    bucket_name.clone(),
                    sample.count as f64,
                    inf_labels,
                    wall_now,
                );
                buf.clear();
                encoder.encode_metric(&event, &mut buf)?;
                total_bytes += buf.len() as u64;
                sink.write(&buf)?;
            }

            // Build labels for _count and _sum (no `le` label).
            let count_sum_labels = if needs_clone {
                let mut bl = (*prebuilt_count_sum_labels).clone();
                for dl in ctx.dynamic_labels {
                    bl.insert(dl.key.clone(), dl.label_value_for_tick(ctx.tick));
                }
                for sw in ctx.spike_windows {
                    if is_in_spike(ctx.elapsed, sw) {
                        bl.insert(sw.label.clone(), sw.label_value_for_tick(ctx.tick));
                    }
                }
                Arc::new(bl)
            } else {
                Arc::clone(&prebuilt_count_sum_labels)
            };

            // Emit _count event.
            let count_event = MetricEvent::from_parts(
                count_name.clone(),
                sample.count as f64,
                Arc::clone(&count_sum_labels),
                wall_now,
            );
            buf.clear();
            encoder.encode_metric(&count_event, &mut buf)?;
            total_bytes += buf.len() as u64;
            sink.write(&buf)?;

            // Emit _sum event.
            let sum_event =
                MetricEvent::from_parts(sum_name.clone(), sample.sum, count_sum_labels, wall_now);
            buf.clear();
            encoder.encode_metric(&sum_event, &mut buf)?;
            total_bytes += buf.len() as u64;
            sink.write(&buf)?;
            let delivered = sink.last_write_delivered();

            Ok(TickResult {
                bytes_written: total_bytes,
                metric_event: Some(count_event),
                delivered,
            })
        };

    let stats_for_flush = stats.clone();
    let loop_result = match gate_ctx {
        None => core_loop::run_schedule_loop(
            &schedule,
            config.rate,
            shutdown,
            stats,
            sink,
            &mut tick_fn,
        ),
        Some(ctx) => core_loop::gated_loop(
            &schedule,
            config.rate,
            shutdown,
            stats,
            ctx,
            sink,
            &mut tick_fn,
        ),
    };

    let flush_result = sink.flush();
    match loop_result {
        Ok(()) => core_loop::apply_flush_policy(&schedule, stats_for_flush.as_ref(), flush_result),
        Err(e) => Err(e),
    }
}

/// Format a bucket boundary as the `le` label value.
///
/// Uses Prometheus conventions: integer values render without decimal point,
/// otherwise uses the default f64 formatting.
fn format_le_value(bound: f64) -> String {
    if bound == bound.trunc() && !bound.is_infinite() {
        // Integer value — format without unnecessary decimal places.
        format!("{}", bound as i64)
    } else {
        format!("{}", bound)
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{BaseScheduleConfig, DistributionConfig, HistogramScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::sink::memory::MemorySink;
    use crate::sink::SinkConfig;

    /// Build a minimal HistogramScenarioConfig for testing.
    fn make_config(
        rate: f64,
        duration: &str,
        buckets: Option<Vec<f64>>,
    ) -> HistogramScenarioConfig {
        HistogramScenarioConfig {
            base: BaseScheduleConfig {
                name: "http_request_duration_seconds".to_string(),
                rate,
                duration: Some(duration.to_string()),
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
            buckets,
            distribution: DistributionConfig::Exponential { rate: 10.0 },
            observations_per_tick: Some(100),
            mean_shift_per_sec: None,
            seed: Some(42),
            encoder: EncoderConfig::PrometheusText { precision: None },
        }
    }

    // ---- Run completes without error ----------------------------------------

    #[test]
    fn run_completes_for_short_duration() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("histogram run must succeed");
        assert!(!sink.buffer.is_empty(), "histogram run must produce output");
    }

    // ---- Output contains expected series names ------------------------------

    #[test]
    fn output_contains_bucket_count_sum_series() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");

        assert!(
            output.contains("http_request_duration_seconds_bucket{"),
            "output must contain _bucket events"
        );
        assert!(
            output.contains("http_request_duration_seconds_count"),
            "output must contain _count events"
        );
        assert!(
            output.contains("http_request_duration_seconds_sum"),
            "output must contain _sum events"
        );
    }

    // ---- le label present on bucket events -----------------------------------

    #[test]
    fn bucket_events_have_le_label() {
        let config = make_config(50.0, "100ms", Some(vec![0.1, 0.5, 1.0]));
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");

        // Check for specific le values.
        assert!(
            output.contains("le=\"0\"") || output.contains("le=\"0.1\""),
            "output must contain le label values"
        );
        assert!(
            output.contains("le=\"+Inf\""),
            "output must contain le=\"+Inf\" bucket"
        );
    }

    // ---- Gap suppresses output -----------------------------------------------

    #[test]
    fn gap_suppresses_histogram_output() {
        let mut config = make_config(100.0, "2s", None);
        config.base.gaps = Some(crate::config::GapConfig {
            every: "1s".to_string(),
            r#for: "500ms".to_string(),
        });

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        // With gaps, we should have less output than without.
        // Just verify it ran successfully and produced some output.
        assert!(
            !sink.buffer.is_empty(),
            "histogram with gaps must still produce some output"
        );
    }

    // ---- Custom buckets are used --------------------------------------------

    #[test]
    fn custom_buckets_appear_in_output() {
        let config = make_config(50.0, "100ms", Some(vec![1.0, 5.0, 10.0]));
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        // Should have events for le="1", le="5", le="10", and le="+Inf"
        assert!(output.contains("le=\"1\""), "expected le=\"1\" in output");
        assert!(
            output.contains("le=\"+Inf\""),
            "expected le=\"+Inf\" in output"
        );
    }

    // ---- format_le_value ----------------------------------------------------

    #[test]
    fn format_le_integer_value() {
        assert_eq!(super::format_le_value(1.0), "1");
        assert_eq!(super::format_le_value(10.0), "10");
    }

    #[test]
    fn format_le_fractional_value() {
        assert_eq!(super::format_le_value(0.005), "0.005");
        assert_eq!(super::format_le_value(0.025), "0.025");
        assert_eq!(super::format_le_value(2.5), "2.5");
    }

    // ---- Labels from config are included ------------------------------------

    #[test]
    fn config_labels_appear_in_output() {
        let mut config = make_config(50.0, "100ms", Some(vec![1.0]));
        let mut label_map = std::collections::HashMap::new();
        label_map.insert("method".to_string(), "GET".to_string());
        config.base.labels = Some(label_map);

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        assert!(
            output.contains("method=\"GET\""),
            "config labels must appear in output"
        );
    }
}
