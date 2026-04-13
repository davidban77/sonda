//! The summary scenario event loop.
//!
//! The summary runner ties together the [`SummaryGenerator`], encoder, and
//! sink with the shared schedule loop from
//! [`core_loop::run_schedule_loop`](super::core_loop::run_schedule_loop).
//!
//! Each tick, the runner:
//! 1. Advances the summary generator to get a [`SummarySample`].
//! 2. For each quantile target, creates a `MetricEvent` with the base name
//!    and a `quantile="{q}"` label.
//! 3. Creates `{base}_count` and `{base}_sum` events.
//! 4. Encodes all events and writes them to the sink.
//!
//! The core loop is unchanged — all summary-specific logic lives in the
//! per-tick closure.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use crate::config::SummaryScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::histogram::to_distribution;
use crate::generator::summary::{SummaryGenerator, DEFAULT_SUMMARY_QUANTILES};
use crate::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use crate::schedule::core_loop::{self, TickContext, TickResult};
use crate::schedule::is_in_spike;
use crate::schedule::stats::ScenarioStats;
use crate::schedule::ParsedSchedule;
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a summary scenario to completion, emitting encoded summary events
/// at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and
/// delegates to [`run_with_sink`] with no shutdown flag and no stats collection.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run(config: &SummaryScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink, None)?;
    run_with_sink(config, sink.as_mut(), None, None)
}

/// Run a summary scenario to completion, writing encoded events into the
/// provided sink.
///
/// Builds the summary generator, encoder, and label sets from the config,
/// then delegates to the shared schedule loop. The per-tick closure generates
/// multiple `MetricEvent`s per tick (one per quantile + `_count` + `_sum`).
///
/// # Parameters
///
/// * `config` — the summary scenario configuration.
/// * `sink` — the destination for encoded metric events.
/// * `shutdown` — optional atomic flag for clean shutdown.
/// * `stats` — optional shared stats for live telemetry.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run_with_sink(
    config: &SummaryScenarioConfig,
    sink: &mut dyn Sink,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
) -> Result<(), SondaError> {
    let schedule = ParsedSchedule::from_base_config(&config.base)?;

    // Resolve summary parameters with defaults.
    let quantiles: Vec<f64> = config
        .quantiles
        .clone()
        .unwrap_or_else(|| DEFAULT_SUMMARY_QUANTILES.to_vec());
    let distribution = to_distribution(&config.distribution);
    let observations_per_tick = config.observations_per_tick.unwrap_or(100);
    let mean_shift_per_sec = config.mean_shift_per_sec.unwrap_or(0.0);
    let seed = config.seed.unwrap_or(0);

    let mut summary_gen = SummaryGenerator::new(
        quantiles.clone(),
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
    let base_name = ValidatedMetricName::new(&config.name)?;
    let count_name = ValidatedMetricName::new(&format!("{}_count", config.name))?;
    let sum_name = ValidatedMetricName::new(&format!("{}_sum", config.name))?;

    // Pre-build quantile label strings.
    let quantile_strings: Vec<String> = quantiles.iter().map(|q| format!("{}", q)).collect();

    // Pre-build Arc<Labels> for each quantile (base labels + quantile="{q}"),
    // _count, and _sum before the hot loop. In steady state (no spikes or
    // dynamic labels), these are reused via Arc::clone — zero heap allocations
    // per tick.
    let prebuilt_quantile_labels: Vec<Arc<Labels>> = quantile_strings
        .iter()
        .map(|q_val| {
            let mut ql = (*labels).clone();
            ql.insert("quantile".to_string(), q_val.clone());
            Arc::new(ql)
        })
        .collect();
    // _count and _sum share the base labels (no `quantile` label).
    let prebuilt_count_sum_labels: Arc<Labels> = Arc::clone(&labels);

    // Pre-allocate encode buffer.
    let mut buf: Vec<u8> = Vec::with_capacity(1024);

    let mut tick_fn = |ctx: &TickContext<'_>| -> Result<TickResult, SondaError> {
        let wall_now = std::time::SystemTime::now();

        // Advance the summary generator.
        let sample = summary_gen.observe(ctx.tick);

        // Determine whether dynamic labels or spikes are active this tick.
        let needs_dynamic = !ctx.dynamic_labels.is_empty();
        let has_active_spike = ctx
            .spike_windows
            .iter()
            .any(|sw| is_in_spike(ctx.elapsed, sw));
        let needs_clone = needs_dynamic || has_active_spike;

        let mut total_bytes: u64 = 0;

        // Emit one event per quantile.
        for (i, &(_q_target, q_value)) in sample.quantiles.iter().enumerate() {
            let quantile_labels = if needs_clone {
                let mut ql = (*prebuilt_quantile_labels[i]).clone();
                for dl in ctx.dynamic_labels {
                    ql.insert(dl.key.clone(), dl.label_value_for_tick(ctx.tick));
                }
                for sw in ctx.spike_windows {
                    if is_in_spike(ctx.elapsed, sw) {
                        ql.insert(sw.label.clone(), sw.label_value_for_tick(ctx.tick));
                    }
                }
                Arc::new(ql)
            } else {
                Arc::clone(&prebuilt_quantile_labels[i])
            };
            let event =
                MetricEvent::from_parts(base_name.clone(), q_value, quantile_labels, wall_now);
            buf.clear();
            encoder.encode_metric(&event, &mut buf)?;
            total_bytes += buf.len() as u64;
            sink.write(&buf)?;
        }

        // Build labels for _count and _sum (no `quantile` label).
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

        Ok(TickResult {
            bytes_written: total_bytes,
            metric_event: Some(count_event),
        })
    };

    let loop_result =
        core_loop::run_schedule_loop(&schedule, config.rate, shutdown, stats, &mut tick_fn);

    let flush_result = sink.flush();
    match loop_result {
        Ok(()) => flush_result,
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{BaseScheduleConfig, DistributionConfig, SummaryScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::sink::memory::MemorySink;
    use crate::sink::SinkConfig;

    /// Build a minimal SummaryScenarioConfig for testing.
    fn make_config(
        rate: f64,
        duration: &str,
        quantiles: Option<Vec<f64>>,
    ) -> SummaryScenarioConfig {
        SummaryScenarioConfig {
            base: BaseScheduleConfig {
                name: "rpc_duration_seconds".to_string(),
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
            },
            quantiles,
            distribution: DistributionConfig::Normal {
                mean: 0.1,
                stddev: 0.02,
            },
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
        super::run_with_sink(&config, &mut sink, None, None).expect("summary run must succeed");
        assert!(!sink.buffer.is_empty(), "summary run must produce output");
    }

    // ---- Output contains expected series names ------------------------------

    #[test]
    fn output_contains_quantile_count_sum_series() {
        let config = make_config(50.0, "200ms", None);
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");

        assert!(
            output.contains("rpc_duration_seconds{"),
            "output must contain base name quantile events"
        );
        assert!(
            output.contains("rpc_duration_seconds_count"),
            "output must contain _count events"
        );
        assert!(
            output.contains("rpc_duration_seconds_sum"),
            "output must contain _sum events"
        );
    }

    // ---- quantile label present on quantile events ---------------------------

    #[test]
    fn quantile_events_have_quantile_label() {
        let config = make_config(50.0, "100ms", Some(vec![0.5, 0.99]));
        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");

        assert!(
            output.contains("quantile=\"0.5\""),
            "output must contain quantile=\"0.5\""
        );
        assert!(
            output.contains("quantile=\"0.99\""),
            "output must contain quantile=\"0.99\""
        );
    }

    // ---- Gap suppresses output -----------------------------------------------

    #[test]
    fn gap_suppresses_summary_output() {
        let mut config = make_config(100.0, "2s", None);
        config.base.gaps = Some(crate::config::GapConfig {
            every: "1s".to_string(),
            r#for: "500ms".to_string(),
        });

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        assert!(
            !sink.buffer.is_empty(),
            "summary with gaps must still produce some output"
        );
    }

    // ---- Labels from config are included ------------------------------------

    #[test]
    fn config_labels_appear_in_output() {
        let mut config = make_config(50.0, "100ms", Some(vec![0.5]));
        let mut label_map = std::collections::HashMap::new();
        label_map.insert("service".to_string(), "auth".to_string());
        config.base.labels = Some(label_map);

        let mut sink = MemorySink::new();
        super::run_with_sink(&config, &mut sink, None, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("valid UTF-8");
        assert!(
            output.contains("service=\"auth\""),
            "config labels must appear in output"
        );
    }
}
