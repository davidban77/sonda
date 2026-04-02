//! The log scenario event loop.
//!
//! Mirrors the structure of [`super::runner`] but drives a `LogGenerator`
//! and calls `Encoder::encode_log` instead of `Encoder::encode_metric`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::validate::parse_duration;
use crate::config::LogScenarioConfig;
use crate::encoder::create_encoder;
use crate::generator::create_log_generator;
use crate::model::metric::Labels;
use crate::schedule::stats::ScenarioStats;
use crate::schedule::{
    is_in_burst, is_in_gap, is_in_spike, time_until_gap_end, BurstWindow, CardinalitySpikeWindow,
    GapWindow,
};
use crate::sink::{create_sink, Sink};
use crate::SondaError;

/// Run a log scenario to completion, emitting encoded log events at the configured rate.
///
/// This is the primary entry point. It constructs a sink from the config and
/// delegates to [`run_logs_with_sink`] with no shutdown flag and no stats collection.
///
/// This function blocks the calling thread until the scenario duration has
/// elapsed. If no duration is specified in the config it runs indefinitely.
///
/// # Errors
///
/// Returns [`SondaError`] if config validation, encoding, or sink I/O fails.
pub fn run_logs(config: &LogScenarioConfig) -> Result<(), SondaError> {
    let mut sink = create_sink(&config.sink, config.labels.as_ref())?;
    run_logs_with_sink(config, sink.as_mut(), None, None)
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
/// * `stats` — an optional shared stats object. When `Some`, the runner updates
///   `total_events`, `bytes_emitted`, `current_rate`, `in_gap`, `in_burst`, and
///   `errors` on each tick. The write lock is held only for the brief counter
///   update, not during encode/write. Pass `None` to skip stats collection with
///   no overhead (e.g., in direct CLI usage or tests).
///
/// # Steps
///
/// 1. Parses the config and builds the log generator and encoder.
/// 2. Enters a tight rate-control loop:
///    - Checks shutdown flag — exits cleanly if cleared.
///    - Checks duration — exits if exceeded.
///    - Checks gap window — sleeps until gap ends if currently in one (gap takes priority over burst).
///    - Checks burst window — uses a shorter effective interval during bursts.
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

    // Build cardinality spike windows from config, if present.
    let spike_windows: Vec<CardinalitySpikeWindow> = config
        .cardinality_spikes
        .as_ref()
        .map(|spikes| {
            spikes
                .iter()
                .map(|s| {
                    Ok(CardinalitySpikeWindow {
                        label: s.label.clone(),
                        every: parse_duration(&s.every)?,
                        duration: parse_duration(&s.r#for)?,
                        cardinality: s.cardinality,
                        strategy: s.strategy,
                        prefix: s.prefix.clone().unwrap_or_else(|| format!("{}_", s.label)),
                        seed: s.seed.unwrap_or(0),
                    })
                })
                .collect::<Result<Vec<_>, SondaError>>()
        })
        .transpose()?
        .unwrap_or_default();

    // Build log generator and encoder from config.
    let generator = create_log_generator(&config.generator)?;
    let encoder = create_encoder(&config.encoder);

    // Build labels from config, mirroring the metrics runner pattern.
    let labels: Labels = if let Some(ref label_map) = config.labels {
        let pairs: Vec<(&str, &str)> = label_map
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Labels::from_pairs(&pairs)?
    } else {
        Labels::default()
    };

    // The base inter-event interval (at normal rate, no burst).
    let base_interval = Duration::from_secs_f64(1.0 / config.rate);

    // Pre-allocate encode buffer — reused every tick to avoid per-event allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(512);

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

            // Generate the log event and inject scenario-level labels.
            let mut event = generator.generate(tick);

            // Inject cardinality spike labels when inside a spike window.
            let currently_in_spike;
            if spike_windows.is_empty() {
                currently_in_spike = false;
                event.labels = labels.clone();
            } else {
                let mut tl = labels.clone();
                let mut any_spike = false;
                for sw in &spike_windows {
                    if is_in_spike(elapsed, sw) {
                        tl.insert(sw.label.clone(), sw.label_value_for_tick(tick));
                        any_spike = true;
                    }
                }
                currently_in_spike = any_spike;
                event.labels = tl;
            }

            // Encode and write.
            buf.clear();
            encoder.encode_log(&event, &mut buf)?;
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
                    s.read().map(|st| st.current_rate).unwrap_or(0.0)
                };

                if let Ok(mut st) = s.write() {
                    st.total_events += 1;
                    st.bytes_emitted += bytes_written;
                    st.current_rate = current_rate;
                    st.in_gap = currently_in_gap;
                    st.in_burst = currently_in_burst;
                    st.in_cardinality_spike = currently_in_spike;
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
    use std::collections::HashMap;

    use super::*;
    use crate::config::{GapConfig, LogScenarioConfig};
    use crate::encoder::EncoderConfig;
    use crate::generator::{LogGeneratorConfig, TemplateConfig};
    use crate::sink::memory::MemorySink;
    use crate::sink::SinkConfig;

    /// Build a minimal valid `LogScenarioConfig` for use in tests.
    ///
    /// Uses the template generator with a static message (no placeholders),
    /// the JSON Lines encoder, and a dummy stdout sink (replaced by tests that
    /// call `run_logs_with_sink` directly).
    fn make_config(rate: f64, duration: Option<&str>) -> LogScenarioConfig {
        LogScenarioConfig {
            name: "test_logs".to_string(),
            rate,
            duration: duration.map(|s| s.to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "synthetic log event".to_string(),
                    field_pools: HashMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::JsonLines { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        }
    }

    // -------------------------------------------------------------------------
    // Integration: MemorySink, rate=10, duration=1s → ~10 encoded log lines
    // -------------------------------------------------------------------------

    /// The log runner must emit approximately `rate` events in `duration` seconds.
    ///
    /// At rate=10 and duration=1s we expect 10 events (within ±3 tolerance to
    /// accommodate OS scheduling jitter without making the test fragile).
    #[test]
    fn run_logs_with_sink_rate_10_duration_1s_produces_approx_10_lines() {
        let config = make_config(10.0, Some("1s"));
        let mut sink = MemorySink::new();

        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        // Count newline-terminated JSON lines.
        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        let line_count = output.lines().count();
        assert!(
            (7..=13).contains(&line_count),
            "expected ~10 log lines, got {line_count}"
        );
    }

    /// Every emitted line must be non-empty valid JSON with a `message` key.
    #[test]
    fn run_logs_with_sink_each_line_is_valid_json() {
        let config = make_config(10.0, Some("1s"));
        let mut sink = MemorySink::new();

        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        for line in output.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
            assert!(
                parsed.get("message").is_some(),
                "each JSON line must contain a 'message' key; line: {line}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Shutdown flag: setting the flag stops the runner before duration expires
    // -------------------------------------------------------------------------

    /// If the shutdown flag is cleared (false) before the scenario would
    /// naturally finish, the runner must exit cleanly without error.
    #[test]
    fn run_logs_with_sink_shutdown_flag_stops_runner() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let config = make_config(5.0, None); // runs indefinitely without shutdown
        let mut sink = MemorySink::new();
        let shutdown = Arc::new(AtomicBool::new(true));

        let flag_clone = Arc::clone(&shutdown);
        // Clear the shutdown flag after 300ms so the runner exits soon.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            flag_clone.store(false, Ordering::SeqCst);
        });

        let result = run_logs_with_sink(&config, &mut sink, Some(shutdown.as_ref()), None);
        assert!(
            result.is_ok(),
            "runner must return Ok when stopped via shutdown flag"
        );
    }

    // -------------------------------------------------------------------------
    // Gap window: events suppressed while in gap
    // -------------------------------------------------------------------------

    /// A gap that covers the entire run duration should produce no output.
    ///
    /// We set gap_every=1s and gap_for=999ms (gap starts at 1ms into the cycle)
    /// and run for 500ms — the scenario starts in a non-gap period initially
    /// but then immediately transitions into the gap for the rest of the run,
    /// so zero or very few events are emitted.
    #[test]
    fn run_logs_with_sink_gap_suppresses_output() {
        // gap: every=10s, for=9s → gap starts at 1s.
        // duration=2s → after 1s of normal events, 1s is spent in a gap.
        let mut config = make_config(100.0, Some("2s"));
        config.gaps = Some(GapConfig {
            every: "10s".to_string(),
            r#for: "9s".to_string(), // gap from second 1 to second 10
        });

        let mut sink = MemorySink::new();
        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("valid UTF-8");
        let line_count = output.lines().count();
        // Only ~100 events from the first second (before the gap). The gap covers
        // seconds 1–10, so the remaining 1s of the 2s run is silent.
        assert!(
            line_count < 150,
            "gap should suppress events: expected < 150 lines, got {line_count}"
        );
    }

    // -------------------------------------------------------------------------
    // Duration=None without shutdown produces no hang (sanity — see note)
    // -------------------------------------------------------------------------

    /// When a finite duration is set, the runner must exit at the right time.
    /// Verify this is respected by running at low rate for 500ms.
    #[test]
    fn run_logs_with_sink_duration_500ms_exits_promptly() {
        use std::time::Instant;

        let config = make_config(5.0, Some("500ms"));
        let mut sink = MemorySink::new();

        let t0 = Instant::now();
        run_logs_with_sink(&config, &mut sink, None, None).expect("must not error");
        let elapsed = t0.elapsed();

        // Should exit within 2 seconds of the 500ms duration.
        assert!(
            elapsed.as_secs() < 2,
            "runner should have exited after ~500ms, elapsed={elapsed:?}"
        );
    }

    // -------------------------------------------------------------------------
    // LogScenarioConfig: YAML deserialization (slice spec test criterion)
    // -------------------------------------------------------------------------

    /// Config from YAML: log-template style YAML → valid `LogScenarioConfig`.
    #[cfg(feature = "config")]
    #[test]
    fn log_scenario_config_deserializes_template_yaml() {
        let yaml = r#"
name: app_logs_template
rate: 10
duration: 60s
generator:
  type: template
  templates:
    - message: "Request from {ip} to {endpoint}"
      field_pools:
        ip:
          - "10.0.0.1"
          - "10.0.0.2"
        endpoint:
          - "/api/v1/health"
          - "/api/v1/metrics"
  severity_weights:
    info: 0.7
    warn: 0.2
    error: 0.1
  seed: 42
encoder:
  type: json_lines
sink:
  type: stdout
"#;
        let config: LogScenarioConfig =
            serde_yaml::from_str(yaml).expect("log-template YAML must deserialize");
        assert_eq!(config.name, "app_logs_template");
        assert_eq!(config.rate, 10.0);
        assert_eq!(config.duration.as_deref(), Some("60s"));
        assert!(matches!(config.encoder, EncoderConfig::JsonLines { .. }));
        assert!(matches!(config.sink, SinkConfig::Stdout));
    }

    /// Config from YAML: log-replay style YAML → valid `LogScenarioConfig`.
    #[cfg(feature = "config")]
    #[test]
    fn log_scenario_config_deserializes_replay_yaml() {
        let yaml = r#"
name: app_logs_replay
rate: 5
duration: 30s
generator:
  type: replay
  file: /var/log/app.log
encoder:
  type: json_lines
sink:
  type: stdout
"#;
        let config: LogScenarioConfig =
            serde_yaml::from_str(yaml).expect("log-replay YAML must deserialize");
        assert_eq!(config.name, "app_logs_replay");
        assert_eq!(config.rate, 5.0);
        assert!(matches!(
            config.generator,
            LogGeneratorConfig::Replay { .. }
        ));
    }

    /// Default encoder for LogScenarioConfig is json_lines (not prometheus_text).
    #[cfg(feature = "config")]
    #[test]
    fn log_scenario_config_default_encoder_is_json_lines() {
        let yaml = r#"
name: defaults_test
rate: 1
generator:
  type: template
  templates:
    - message: "hello"
      field_pools: {}
"#;
        let config: LogScenarioConfig =
            serde_yaml::from_str(yaml).expect("minimal log YAML must deserialize");
        assert!(
            matches!(config.encoder, EncoderConfig::JsonLines { .. }),
            "default encoder must be json_lines, got {:?}",
            config.encoder
        );
    }

    /// Default sink for LogScenarioConfig is stdout.
    #[cfg(feature = "config")]
    #[test]
    fn log_scenario_config_default_sink_is_stdout() {
        let yaml = r#"
name: defaults_test
rate: 1
generator:
  type: template
  templates:
    - message: "hello"
      field_pools: {}
"#;
        let config: LogScenarioConfig =
            serde_yaml::from_str(yaml).expect("minimal log YAML must deserialize");
        assert!(
            matches!(config.sink, SinkConfig::Stdout),
            "default sink must be stdout, got {:?}",
            config.sink
        );
    }

    /// LogScenarioConfig with optional gaps and bursts deserializes correctly.
    #[cfg(feature = "config")]
    #[test]
    fn log_scenario_config_with_gaps_and_bursts_deserializes() {
        let yaml = r#"
name: full_config
rate: 20
duration: 120s
generator:
  type: template
  templates:
    - message: "event"
      field_pools: {}
gaps:
  every: 10s
  for: 2s
bursts:
  every: 5s
  for: 1s
  multiplier: 10.0
encoder:
  type: syslog
  hostname: myhost
  app_name: myapp
sink:
  type: stdout
"#;
        let config: LogScenarioConfig =
            serde_yaml::from_str(yaml).expect("full log YAML must deserialize");
        let gaps = config.gaps.as_ref().expect("gaps must be present");
        assert_eq!(gaps.every, "10s");
        assert_eq!(gaps.r#for, "2s");
        let bursts = config.bursts.as_ref().expect("bursts must be present");
        assert_eq!(bursts.every, "5s");
        assert_eq!(bursts.r#for, "1s");
        assert_eq!(bursts.multiplier, 10.0);
    }

    // -------------------------------------------------------------------------
    // Contract: LogScenarioConfig is Clone + Debug
    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------
    // Labels: scenario-level labels appear in encoded JSON output
    // -------------------------------------------------------------------------

    /// When labels are configured, every emitted JSON line must include the
    /// labels object with the correct key-value pairs.
    #[test]
    fn run_logs_with_sink_labels_appear_in_json_output() {
        let mut config = make_config(10.0, Some("1s"));
        let mut label_map = HashMap::new();
        label_map.insert("device".to_string(), "wlan0".to_string());
        label_map.insert("hostname".to_string(), "router_01".to_string());
        config.labels = Some(label_map);

        let mut sink = MemorySink::new();
        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            !lines.is_empty(),
            "runner must produce at least one line of output"
        );

        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
            assert_eq!(
                parsed["labels"]["device"], "wlan0",
                "every JSON line must contain label device=wlan0; line: {line}"
            );
            assert_eq!(
                parsed["labels"]["hostname"], "router_01",
                "every JSON line must contain label hostname=router_01; line: {line}"
            );
        }
    }

    /// When no labels are configured, the labels object in JSON output must be
    /// empty (not absent).
    #[test]
    fn run_logs_with_sink_no_labels_produces_empty_labels_object() {
        let config = make_config(10.0, Some("500ms"));
        let mut sink = MemorySink::new();
        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        for line in output.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
            assert_eq!(
                parsed["labels"],
                serde_json::json!({}),
                "when no labels configured, labels must be empty object; line: {line}"
            );
        }
    }

    /// Labels in syslog encoder should appear as structured data.
    #[test]
    fn run_logs_with_sink_labels_appear_in_syslog_output() {
        let mut config = make_config(10.0, Some("500ms"));
        config.encoder = EncoderConfig::Syslog {
            hostname: None,
            app_name: None,
        };
        let mut label_map = HashMap::new();
        label_map.insert("env".to_string(), "prod".to_string());
        config.labels = Some(label_map);

        let mut sink = MemorySink::new();
        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            !lines.is_empty(),
            "runner must produce at least one syslog line"
        );

        for line in &lines {
            assert!(
                line.contains("[sonda env=\"prod\"]"),
                "every syslog line must contain structured data with labels; line: {line}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Contract: LogScenarioConfig is Clone + Debug
    // -------------------------------------------------------------------------

    #[test]
    fn log_scenario_config_is_clone_and_debug() {
        let config = make_config(10.0, Some("1s"));
        let cloned = config.clone();
        assert_eq!(cloned.name, config.name);
        assert_eq!(cloned.rate, config.rate);
        let s = format!("{config:?}");
        assert!(s.contains("LogScenarioConfig") || s.contains("test_logs"));
    }

    // -------------------------------------------------------------------------
    // Cardinality spikes: labels appear in JSON output during spike window
    // -------------------------------------------------------------------------

    /// Helper that builds a LogScenarioConfig with a cardinality spike.
    fn make_config_with_spike(
        rate: f64,
        duration: Option<&str>,
        spike: crate::config::CardinalitySpikeConfig,
    ) -> LogScenarioConfig {
        let mut config = make_config(rate, duration);
        config.cardinality_spikes = Some(vec![spike]);
        config
    }

    /// When the entire run is inside a spike window, every JSON line must
    /// contain the spike label key in the labels object.
    #[test]
    fn run_logs_with_sink_spike_labels_appear_during_spike_window() {
        let spike = crate::config::CardinalitySpikeConfig {
            label: "pod_name".to_string(),
            every: "10s".to_string(),
            r#for: "9s".to_string(),
            cardinality: 5,
            strategy: crate::config::SpikeStrategy::Counter,
            prefix: Some("pod-".to_string()),
            seed: None,
        };
        let config = make_config_with_spike(10.0, Some("1s"), spike);
        let mut sink = MemorySink::new();

        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        let lines: Vec<&str> = output.lines().collect();
        assert!(
            !lines.is_empty(),
            "runner must produce at least one line of output"
        );

        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
            assert!(
                parsed["labels"]["pod_name"].is_string(),
                "every JSON line during spike must contain pod_name label; line: {line}"
            );
            let pod_val = parsed["labels"]["pod_name"].as_str().unwrap();
            assert!(
                pod_val.starts_with("pod-"),
                "spike label value must start with prefix 'pod-', got: {pod_val}"
            );
        }
    }

    /// When no spike windows are configured, labels object must not contain spike keys.
    #[test]
    fn run_logs_with_sink_no_spike_config_produces_no_spike_labels() {
        let config = make_config(10.0, Some("500ms"));
        let mut sink = MemorySink::new();
        run_logs_with_sink(&config, &mut sink, None, None).expect("log runner must not error");

        let output = String::from_utf8(sink.buffer.clone()).expect("output must be valid UTF-8");
        for line in output.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
            assert!(
                parsed["labels"]["pod_name"].is_null(),
                "without spike config, pod_name must not appear in labels; line: {line}"
            );
        }
    }
}
