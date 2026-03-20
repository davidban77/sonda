//! The main scenario event loop.
//!
//! The runner ties together all sonda-core components: it reads a
//! [`ScenarioConfig`], builds the generator, encoder, and sink, then drives the
//! tight rate-controlled loop that emits encoded metric events.

use std::thread;
use std::time::{Duration, Instant};

use crate::config::validate::parse_duration;
use crate::config::ScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_generator;
use crate::model::metric::{Labels, MetricEvent};
use crate::schedule::{is_in_gap, time_until_gap_end, GapWindow};
use crate::sink::create_sink;
use crate::SondaError;

/// Run a scenario to completion, emitting encoded metric events at the configured rate.
///
/// This function blocks the calling thread until the scenario duration has
/// elapsed. If no duration is specified in the config it runs indefinitely.
///
/// # Steps
///
/// 1. Parses the config and builds the generator, encoder, and sink.
/// 2. Builds the [`Labels`] set from the config label map.
/// 3. Enters a tight rate-control loop:
///    - Checks duration — exits if exceeded.
///    - Checks gap window — sleeps until gap ends if currently in one.
///    - Generates a value, builds a [`MetricEvent`], encodes it, writes to sink.
///    - Sleeps for the remaining inter-event interval (accounting for elapsed work).
/// 4. Flushes the sink before returning.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run(config: &ScenarioConfig) -> Result<(), SondaError> {
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

    // Build generator, encoder, and sink from config.
    let generator = create_generator(&config.generator, config.rate);
    let encoder = create_encoder(&config.encoder);
    let mut sink = create_sink(&config.sink)?;

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

    // The target inter-event interval.
    let interval = Duration::from_secs_f64(1.0 / config.rate);

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    let start = Instant::now();
    let mut tick: u64 = 0;

    loop {
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
                // After sleeping, re-check duration before emitting.
                continue;
            }
        }

        // Timestamp the event at the start of this iteration.
        let now = std::time::SystemTime::now();

        // Generate the value and build the metric event.
        let value = generator.value(tick);
        let event = MetricEvent::with_timestamp(config.name.clone(), value, labels.clone(), now)?;

        // Encode and write.
        buf.clear();
        encoder.encode_metric(&event, &mut buf)?;
        sink.write(&buf)?;

        tick += 1;

        // Rate control: sleep for whatever time remains in this interval.
        let iteration_elapsed = start.elapsed() - elapsed;
        if interval > iteration_elapsed {
            thread::sleep(interval - iteration_elapsed);
        }
    }

    // Flush any buffered data before returning.
    sink.flush()?;

    Ok(())
}
