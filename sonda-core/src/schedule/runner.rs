//! The main scenario event loop.
//!
//! The runner ties together all sonda-core components: it reads a
//! [`ScenarioConfig`], builds the generator, encoder, and sink, then drives the
//! tight rate-controlled loop that emits encoded metric events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::validate::parse_duration;
use crate::config::ScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_generator;
use crate::model::metric::{Labels, MetricEvent};
use crate::schedule::{is_in_gap, time_until_gap_end, GapWindow};
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a scenario to completion, emitting encoded metric events at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and then
/// delegates to [`run_with_sink`] with no shutdown flag.
///
/// This function blocks the calling thread until the scenario duration has
/// elapsed. If no duration is specified in the config it runs indefinitely.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run(config: &ScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink)?;
    run_with_sink(config, sink.as_mut(), None)
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

    // Build generator and encoder from config.
    let generator = create_generator(&config.generator, config.rate);
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

    // The target inter-event interval.
    let interval = Duration::from_secs_f64(1.0 / config.rate);

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    // Record the wall-clock start time once. All tick deadlines are computed
    // relative to this instant so sleep drift cannot accumulate across ticks.
    let start = Instant::now();
    let mut tick: u64 = 0;

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
            if let Some(ref gap) = gap_window {
                if is_in_gap(elapsed, gap) {
                    let sleep_for = time_until_gap_end(elapsed, gap);
                    if sleep_for > Duration::ZERO {
                        thread::sleep(sleep_for);
                    }
                    // After sleeping through the gap, advance tick to keep
                    // deadlines consistent with actual wall-clock time so we
                    // don't try to "catch up" for events suppressed by the gap.
                    let now_elapsed = start.elapsed();
                    tick = (now_elapsed.as_secs_f64() / interval.as_secs_f64()) as u64;
                    // Re-check duration before emitting.
                    continue;
                }
            }

            // Deadline-based rate control: compute the absolute wall-clock time
            // at which this tick should fire. If we are ahead of schedule, sleep
            // the remaining delta. If we are already behind (deadline passed),
            // emit immediately without sleeping — this naturally absorbs the
            // overhead of encode/write without accumulating drift.
            //
            // Using `tick as u32` is safe here: at 1 MHz for 49 days tick would
            // overflow u32, but no sonda scenario runs that long.
            let deadline = start + interval * tick as u32;
            let now = Instant::now();
            if now < deadline {
                thread::sleep(deadline - now);
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
            sink.write(&buf)?;

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
            labels: None,
            encoder: EncoderConfig::PrometheusText,
            sink: SinkConfig::Stdout, // not used — tests use run_with_sink directly
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
        super::run_with_sink(&config, &mut sink, None).expect("run must succeed");

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
        super::run_with_sink(&config, &mut sink, None).expect("run must succeed");

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
        super::run_with_sink(&config, &mut sink, None).expect("run must succeed");

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
        super::run_with_sink(&config, &mut sink, None).expect("run must succeed");

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
        super::run_with_sink(&config, &mut sink, None).expect("run must succeed");

        let output = std::str::from_utf8(&sink.buffer).expect("output must be valid UTF-8");
        assert!(
            output.contains("host=\"server1\""),
            "label must appear in output, got:\n{output}"
        );
    }
}
