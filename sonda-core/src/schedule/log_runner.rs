//! The log scenario event loop.
//!
//! Mirrors the structure of [`super::runner`] but drives a [`LogGenerator`]
//! and calls [`Encoder::encode_log`] instead of [`Encoder::encode_metric`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::validate::parse_duration;
use crate::config::LogScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_log_generator;
use crate::schedule::{is_in_gap, time_until_gap_end, GapWindow};
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a log scenario to completion, emitting encoded log events at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and
/// delegates to [`run_logs_with_sink`] with no shutdown flag.
///
/// This function blocks the calling thread until the scenario duration has
/// elapsed. If no duration is specified in the config it runs indefinitely.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run_logs(config: &LogScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink)?;
    run_logs_with_sink(config, sink.as_mut(), None)
}

/// Run a log scenario to completion, writing encoded events into the provided sink.
///
/// This function is the core log event loop implementation. It accepts any
/// [`Sink`] implementation, enabling tests to use a
/// [`MemorySink`](crate::sink::memory::MemorySink) instead of the
/// config-specified sink.
///
/// # Parameters
///
/// * `config` — the log scenario configuration.
/// * `sink` — the destination for encoded log events.
/// * `shutdown` — an optional atomic flag; when set to `false` the loop exits
///   cleanly after the current tick, flushes the sink, and returns `Ok(())`.
///   Pass `None` if no external shutdown signal is needed (e.g., in tests).
///
/// # Steps
///
/// 1. Parses the config and builds the log generator and encoder.
/// 2. Enters a tight rate-control loop:
///    - Checks shutdown flag — exits cleanly if cleared.
///    - Checks duration — exits if exceeded.
///    - Checks gap window — sleeps until gap ends if currently in one.
///    - Generates a log event, encodes it, writes to sink.
///    - Sleeps for the remaining inter-event interval (accounting for elapsed work).
/// 3. Flushes the sink before returning, even if the loop exited via an error.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
/// If an error occurs during the loop and flushing also fails, the loop error
/// is returned (the flush error is discarded to preserve the original cause).
pub fn run_logs_with_sink(
    config: &LogScenarioConfig,
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

    // Build log generator and encoder from config.
    let generator = create_log_generator(&config.generator)?;
    let encoder = create_encoder(&config.encoder);

    // The target inter-event interval.
    let interval = Duration::from_secs_f64(1.0 / config.rate);

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(512);

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
            let deadline = start + interval.mul_f64(tick as f64);
            let now = Instant::now();
            if now < deadline {
                thread::sleep(deadline - now);
            }

            // Generate the log event.
            let event = generator.generate(tick);

            // Encode and write.
            buf.clear();
            encoder.encode_log(&event, &mut buf)?;
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
