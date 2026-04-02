//! Colored lifecycle banners for CLI status output.
//!
//! All output goes to stderr so that stdout remains clean for data (encoded
//! events). The [`print_start`] and [`print_stop`] functions are no-ops when
//! verbosity is [`Verbosity::Quiet`]. The [`print_config`] function displays
//! the resolved scenario config in a human-readable format. The
//! [`print_summary`] function prints an aggregate summary after all scenarios
//! complete in the `run` subcommand.

use std::time::Duration;

use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use crate::cli::Verbosity;
use sonda_core::config::{
    BurstConfig, CardinalitySpikeConfig, GapConfig, LogScenarioConfig, ScenarioConfig,
    ScenarioEntry,
};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::SinkConfig;

/// Print a start banner for a scenario to stderr.
///
/// Displays the scenario name, signal type, rate, encoder, sink, and optional
/// duration. Returns immediately if verbosity is [`Verbosity::Quiet`].
pub fn print_start(entry: &ScenarioEntry, verbosity: Verbosity) {
    if verbosity == Verbosity::Quiet {
        return;
    }

    let (name, signal_type, rate, encoder, sink, duration) = match entry {
        ScenarioEntry::Metrics(c) => (
            c.name.as_str(),
            "metrics",
            c.rate,
            encoder_display(&c.encoder),
            sink_display(&c.sink),
            c.duration.as_deref(),
        ),
        ScenarioEntry::Logs(c) => (
            c.name.as_str(),
            "logs",
            c.rate,
            encoder_display(&c.encoder),
            sink_display(&c.sink),
            c.duration.as_deref(),
        ),
    };

    let arrow = "\u{25b6}".if_supports_color(Stderr, |t| t.green());
    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let signal_label = "signal_type:".if_supports_color(Stderr, |t| t.dimmed());
    let rate_label = "rate:".if_supports_color(Stderr, |t| t.dimmed());
    let encoder_label = "encoder:".if_supports_color(Stderr, |t| t.dimmed());
    let sink_label = "sink:".if_supports_color(Stderr, |t| t.dimmed());

    let rate_str = format_rate(rate);

    match duration {
        Some(dur) => {
            let dur_label = "duration:".if_supports_color(Stderr, |t| t.dimmed());
            eprintln!(
                "{arrow} {bold_name}  {signal_label} {signal_type} {pipe} {rate_label} {rate_str}/s {pipe} {encoder_label} {encoder} {pipe} {sink_label} {sink} {pipe} {dur_label} {dur}"
            );
        }
        None => {
            eprintln!(
                "{arrow} {bold_name}  {signal_label} {signal_type} {pipe} {rate_label} {rate_str}/s {pipe} {encoder_label} {encoder} {pipe} {sink_label} {sink}"
            );
        }
    }
}

/// Print a stop banner for a scenario to stderr.
///
/// Displays the scenario name, elapsed time, total events, bytes emitted, and
/// error count. The stop icon is colored blue normally, or yellow if there were
/// errors. The error count is red when non-zero. Returns immediately if
/// verbosity is [`Verbosity::Quiet`].
pub fn print_stop(name: &str, elapsed: Duration, stats: &ScenarioStats, verbosity: Verbosity) {
    if verbosity == Verbosity::Quiet {
        return;
    }

    let has_errors = stats.errors > 0;

    let square = if has_errors {
        format!("{}", "\u{25a0}".if_supports_color(Stderr, |t| t.yellow()))
    } else {
        format!("{}", "\u{25a0}".if_supports_color(Stderr, |t| t.blue()))
    };

    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let elapsed_secs = elapsed.as_secs_f64();
    let elapsed_str = format!("{elapsed_secs:.1}s");
    let bytes_str = format_bytes(stats.bytes_emitted);

    let events_label = "events:".if_supports_color(Stderr, |t| t.dimmed());
    let bytes_label = "bytes:".if_supports_color(Stderr, |t| t.dimmed());
    let errors_label = "errors:".if_supports_color(Stderr, |t| t.dimmed());

    let errors_value = if has_errors {
        format!("{}", stats.errors.if_supports_color(Stderr, |t| t.red()))
    } else {
        format!("{}", stats.errors)
    };

    eprintln!(
        "{square} {bold_name}  completed in {elapsed_str} {pipe} {events_label} {} {pipe} {bytes_label} {bytes_str} {pipe} {errors_label} {errors_value}",
        stats.total_events
    );
}

/// Print the resolved config for a single scenario entry to stderr.
///
/// Used by `--dry-run` (always) and `--verbose` (at startup). The output is
/// human-readable custom formatting, not YAML or Debug output.
pub fn print_config(entry: &ScenarioEntry) {
    match entry {
        ScenarioEntry::Metrics(c) => print_metrics_config(c),
        ScenarioEntry::Logs(c) => print_logs_config(c),
    }
}

/// Print the resolved config for a metrics scenario.
fn print_metrics_config(c: &ScenarioConfig) {
    let header = "[config]".if_supports_color(Stderr, |t| t.cyan());
    eprintln!("{header} Resolved scenario config:");
    eprintln!();
    eprintln!("  name:       {}", c.name);
    eprintln!("  signal:     metrics");
    eprintln!("  rate:       {}/s", format_rate(c.rate));
    eprintln!(
        "  duration:   {}",
        c.duration.as_deref().unwrap_or("indefinite")
    );
    eprintln!("  generator:  {}", generator_display(&c.generator));
    eprintln!("  encoder:    {}", encoder_display(&c.encoder));
    eprintln!("  sink:       {}", sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    eprintln!();
}

/// Print the resolved config for a logs scenario.
fn print_logs_config(c: &LogScenarioConfig) {
    let header = "[config]".if_supports_color(Stderr, |t| t.cyan());
    eprintln!("{header} Resolved scenario config:");
    eprintln!();
    eprintln!("  name:       {}", c.name);
    eprintln!("  signal:     logs");
    eprintln!("  rate:       {}/s", format_rate(c.rate));
    eprintln!(
        "  duration:   {}",
        c.duration.as_deref().unwrap_or("indefinite")
    );
    eprintln!("  generator:  {}", log_generator_display(&c.generator));
    eprintln!("  encoder:    {}", encoder_display(&c.encoder));
    eprintln!("  sink:       {}", sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    eprintln!();
}

/// Print the labels line if labels are present and non-empty.
fn print_labels_line(labels: &Option<std::collections::HashMap<String, String>>) {
    if let Some(ref map) = labels {
        if !map.is_empty() {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| (*k).clone());
            let formatted: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            eprintln!("  labels:     {}", formatted.join(", "));
        }
    }
}

/// Print the gaps line if gap config is present.
fn print_gaps_line(gaps: &Option<GapConfig>) {
    if let Some(ref g) = gaps {
        eprintln!("  gaps:       every {}, for {}", g.every, g.r#for);
    }
}

/// Print the bursts line if burst config is present.
fn print_bursts_line(bursts: &Option<BurstConfig>) {
    if let Some(ref b) = bursts {
        eprintln!(
            "  bursts:     every {}, for {}, multiplier {}x",
            b.every, b.r#for, b.multiplier
        );
    }
}

/// Print cardinality spike lines if spikes are configured.
fn print_spikes_lines(spikes: &Option<Vec<CardinalitySpikeConfig>>) {
    if let Some(ref list) = spikes {
        for s in list {
            eprintln!(
                "  spikes:     label={}, every {}, for {}, cardinality={}",
                s.label, s.every, s.r#for, s.cardinality
            );
        }
    }
}

/// Print the dry-run validation result to stderr.
///
/// Called after all entries are printed in dry-run mode to confirm that
/// validation passed.
pub fn print_dry_run_ok() {
    let ok_label = "OK".if_supports_color(Stderr, |t| t.green());
    eprintln!("Validation: {ok_label}");
}

/// Aggregate stats for the `run` subcommand summary line.
///
/// Collected from individual scenario handles after all complete.
pub struct AggregateStats {
    /// Number of scenarios that ran.
    pub scenario_count: usize,
    /// Total events across all scenarios.
    pub total_events: u64,
    /// Total bytes emitted across all scenarios.
    pub total_bytes: u64,
    /// Total errors across all scenarios.
    pub total_errors: u64,
}

/// Print an aggregate summary line for the `run` subcommand to stderr.
///
/// Only printed for multi-scenario runs. Single-scenario `metrics`/`logs`
/// commands already have adequate stop banners. Returns immediately if
/// verbosity is [`Verbosity::Quiet`].
pub fn print_summary(agg: &AggregateStats, total_elapsed: Duration, verbosity: Verbosity) {
    if verbosity == Verbosity::Quiet {
        return;
    }

    let bar = "\u{2501}\u{2501}".if_supports_color(Stderr, |t| t.bold());
    let label = "run complete".if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let elapsed_str = format!("{:.1}s", total_elapsed.as_secs_f64());
    let bytes_str = format_bytes(agg.total_bytes);

    let errors_value = if agg.total_errors > 0 {
        format!(
            "{}",
            agg.total_errors.if_supports_color(Stderr, |t| t.red())
        )
    } else {
        format!("{}", agg.total_errors)
    };

    eprintln!(
        "{bar} {label}  scenarios: {} {pipe} events: {} {pipe} bytes: {bytes_str} {pipe} errors: {errors_value} {pipe} elapsed: {elapsed_str}",
        agg.scenario_count, agg.total_events
    );
}

/// Format a metrics generator config as a human-readable display string.
fn generator_display(gen: &GeneratorConfig) -> String {
    match gen {
        GeneratorConfig::Constant { value } => format!("constant (value: {value})"),
        GeneratorConfig::Uniform { min, max, seed } => {
            let seed_str = seed.map(|s| format!(", seed: {s}")).unwrap_or_default();
            format!("uniform (min: {min}, max: {max}{seed_str})")
        }
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => format!("sine (amplitude: {amplitude}, period: {period_secs}s, offset: {offset})"),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => format!("sawtooth (min: {min}, max: {max}, period: {period_secs}s)"),
        GeneratorConfig::Sequence { values, repeat } => {
            let repeat_str = if repeat.unwrap_or(true) {
                "repeat"
            } else {
                "clamp"
            };
            if values.len() <= 5 {
                format!("sequence ({values:?}, {repeat_str})")
            } else {
                format!(
                    "sequence ([{}, {}, ... {} total], {repeat_str})",
                    values[0],
                    values[1],
                    values.len()
                )
            }
        }
        GeneratorConfig::CsvReplay {
            file,
            column,
            has_header,
            repeat,
        } => {
            let col = column.unwrap_or(0);
            let hdr = if has_header.unwrap_or(true) {
                "header"
            } else {
                "no header"
            };
            let rpt = if repeat.unwrap_or(true) {
                "repeat"
            } else {
                "clamp"
            };
            format!("csv_replay (file: {file}, column: {col}, {hdr}, {rpt})")
        }
    }
}

/// Format a log generator config as a human-readable display string.
fn log_generator_display(gen: &LogGeneratorConfig) -> String {
    match gen {
        LogGeneratorConfig::Template {
            templates,
            severity_weights,
            seed,
        } => {
            let tmpl_count = templates.len();
            let seed_str = seed.map(|s| format!(", seed: {s}")).unwrap_or_default();
            let weights_str = if let Some(ref w) = severity_weights {
                let mut pairs: Vec<_> = w.iter().collect();
                pairs.sort_by_key(|(k, _)| (*k).clone());
                let formatted: Vec<String> =
                    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!(", severity: {}", formatted.join("/"))
            } else {
                String::new()
            };
            format!("template ({tmpl_count} template(s){weights_str}{seed_str})")
        }
        LogGeneratorConfig::Replay { file } => format!("replay (file: {file})"),
    }
}

/// Format a sink config as a human-readable display string.
fn sink_display(sink: &SinkConfig) -> String {
    match sink {
        SinkConfig::Stdout => "stdout".to_string(),
        SinkConfig::File { path } => format!("file: {path}"),
        SinkConfig::Tcp { address } => format!("tcp: {address}"),
        SinkConfig::Udp { address } => format!("udp: {address}"),
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { url, .. } => format!("http: {url}"),
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, .. } => format!("remote_write: {url}"),
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { topic, .. } => format!("kafka: {topic}"),
        #[cfg(feature = "http")]
        SinkConfig::Loki { url, .. } => format!("loki: {url}"),
    }
}

/// Format an encoder config as a human-readable display string.
///
/// Includes the precision suffix when set (e.g. `"prometheus_text (precision: 2)"`).
fn encoder_display(encoder: &EncoderConfig) -> String {
    let (name, precision) = match encoder {
        EncoderConfig::PrometheusText { precision } => ("prometheus_text", *precision),
        EncoderConfig::InfluxLineProtocol { precision, .. } => ("influx_lp", *precision),
        EncoderConfig::JsonLines { precision } => ("json_lines", *precision),
        EncoderConfig::Syslog { .. } => ("syslog", None),
        #[cfg(feature = "remote-write")]
        EncoderConfig::RemoteWrite => ("remote_write", None),
    };
    match precision {
        Some(p) => format!("{name} (precision: {p})"),
        None => name.to_string(),
    }
}

/// Format a byte count as a human-readable string with appropriate units.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;

    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    }
}

/// Format a rate value as a string, showing as integer when it is a whole number.
fn format_rate(rate: f64) -> String {
    if rate.fract() == 0.0 && rate.is_finite() {
        format!("{}", rate as u64)
    } else {
        format!("{rate:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::time::Duration;

    use sonda_core::config::{LogScenarioConfig, ScenarioConfig};
    use sonda_core::encoder::EncoderConfig;
    use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
    use sonda_core::schedule::stats::ScenarioStats;
    use sonda_core::sink::SinkConfig;

    // -----------------------------------------------------------------------
    // format_bytes: all unit thresholds
    // -----------------------------------------------------------------------

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_below_kb() {
        assert_eq!(format_bytes(500), "500 B");
    }

    #[test]
    fn format_bytes_exactly_one_kb() {
        assert_eq!(format_bytes(1024), "1.0 KB");
    }

    #[test]
    fn format_bytes_one_and_half_kb() {
        assert_eq!(format_bytes(1536), "1.5 KB");
    }

    #[test]
    fn format_bytes_exactly_one_mb() {
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
    }

    #[test]
    fn format_bytes_one_and_half_mb() {
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }

    #[test]
    fn format_bytes_exactly_one_gb() {
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn format_bytes_one_byte() {
        assert_eq!(format_bytes(1), "1 B");
    }

    #[test]
    fn format_bytes_max_before_kb() {
        assert_eq!(format_bytes(1023), "1023 B");
    }

    // -----------------------------------------------------------------------
    // format_rate: integer vs decimal
    // -----------------------------------------------------------------------

    #[test]
    fn format_rate_whole_number() {
        assert_eq!(format_rate(1000.0), "1000");
    }

    #[test]
    fn format_rate_fractional() {
        assert_eq!(format_rate(0.5), "0.5");
    }

    #[test]
    fn format_rate_one_point_zero() {
        assert_eq!(format_rate(1.0), "1");
    }

    #[test]
    fn format_rate_ten_and_half() {
        assert_eq!(format_rate(10.5), "10.5");
    }

    #[test]
    fn format_rate_zero() {
        assert_eq!(format_rate(0.0), "0");
    }

    // -----------------------------------------------------------------------
    // sink_display: all SinkConfig variants
    // -----------------------------------------------------------------------

    #[test]
    fn sink_display_stdout() {
        assert_eq!(sink_display(&SinkConfig::Stdout), "stdout");
    }

    #[test]
    fn sink_display_file() {
        let config = SinkConfig::File {
            path: "/tmp/out.txt".to_string(),
        };
        assert_eq!(sink_display(&config), "file: /tmp/out.txt");
    }

    #[test]
    fn sink_display_tcp() {
        let config = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
        };
        assert_eq!(sink_display(&config), "tcp: 127.0.0.1:9999");
    }

    #[test]
    fn sink_display_udp() {
        let config = SinkConfig::Udp {
            address: "127.0.0.1:8888".to_string(),
        };
        assert_eq!(sink_display(&config), "udp: 127.0.0.1:8888");
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_display_http_push() {
        let config = SinkConfig::HttpPush {
            url: "http://localhost:9090/write".to_string(),
            content_type: None,
            batch_size: None,
            headers: None,
        };
        assert_eq!(sink_display(&config), "http: http://localhost:9090/write");
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_display_loki() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100/loki/api/v1/push".to_string(),
            batch_size: None,
        };
        assert_eq!(
            sink_display(&config),
            "loki: http://localhost:3100/loki/api/v1/push"
        );
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn sink_display_remote_write() {
        let config = SinkConfig::RemoteWrite {
            url: "http://localhost:8428/api/v1/write".to_string(),
            batch_size: None,
        };
        assert_eq!(
            sink_display(&config),
            "remote_write: http://localhost:8428/api/v1/write"
        );
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_display_kafka() {
        let config = SinkConfig::Kafka {
            brokers: "127.0.0.1:9092".to_string(),
            topic: "sonda-events".to_string(),
        };
        assert_eq!(sink_display(&config), "kafka: sonda-events");
    }

    // -----------------------------------------------------------------------
    // encoder_display: all EncoderConfig variants
    // -----------------------------------------------------------------------

    #[test]
    fn encoder_display_prometheus_text() {
        assert_eq!(
            encoder_display(&EncoderConfig::PrometheusText { precision: None }),
            "prometheus_text"
        );
    }

    #[test]
    fn encoder_display_prometheus_text_with_precision() {
        assert_eq!(
            encoder_display(&EncoderConfig::PrometheusText { precision: Some(2) }),
            "prometheus_text (precision: 2)"
        );
    }

    #[test]
    fn encoder_display_influx_lp_without_field_key() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        };
        assert_eq!(encoder_display(&config), "influx_lp");
    }

    #[test]
    fn encoder_display_influx_lp_with_field_key() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("bytes".to_string()),
            precision: None,
        };
        assert_eq!(encoder_display(&config), "influx_lp");
    }

    #[test]
    fn encoder_display_influx_lp_with_precision() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: Some(4),
        };
        assert_eq!(encoder_display(&config), "influx_lp (precision: 4)");
    }

    #[test]
    fn encoder_display_json_lines() {
        assert_eq!(
            encoder_display(&EncoderConfig::JsonLines { precision: None }),
            "json_lines"
        );
    }

    #[test]
    fn encoder_display_json_lines_with_precision() {
        assert_eq!(
            encoder_display(&EncoderConfig::JsonLines { precision: Some(3) }),
            "json_lines (precision: 3)"
        );
    }

    #[test]
    fn encoder_display_syslog() {
        let config = EncoderConfig::Syslog {
            hostname: None,
            app_name: None,
        };
        assert_eq!(encoder_display(&config), "syslog");
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn encoder_display_remote_write() {
        assert_eq!(encoder_display(&EncoderConfig::RemoteWrite), "remote_write");
    }

    // -----------------------------------------------------------------------
    // generator_display: all GeneratorConfig variants
    // -----------------------------------------------------------------------

    #[test]
    fn generator_display_constant() {
        let config = GeneratorConfig::Constant { value: 42.0 };
        assert_eq!(generator_display(&config), "constant (value: 42)");
    }

    #[test]
    fn generator_display_uniform_with_seed() {
        let config = GeneratorConfig::Uniform {
            min: 0.0,
            max: 100.0,
            seed: Some(7),
        };
        assert_eq!(
            generator_display(&config),
            "uniform (min: 0, max: 100, seed: 7)"
        );
    }

    #[test]
    fn generator_display_uniform_without_seed() {
        let config = GeneratorConfig::Uniform {
            min: 1.5,
            max: 9.5,
            seed: None,
        };
        assert_eq!(generator_display(&config), "uniform (min: 1.5, max: 9.5)");
    }

    #[test]
    fn generator_display_sine() {
        let config = GeneratorConfig::Sine {
            amplitude: 50.0,
            period_secs: 60.0,
            offset: 50.0,
        };
        assert_eq!(
            generator_display(&config),
            "sine (amplitude: 50, period: 60s, offset: 50)"
        );
    }

    #[test]
    fn generator_display_sawtooth() {
        let config = GeneratorConfig::Sawtooth {
            min: 0.0,
            max: 100.0,
            period_secs: 30.0,
        };
        assert_eq!(
            generator_display(&config),
            "sawtooth (min: 0, max: 100, period: 30s)"
        );
    }

    #[test]
    fn generator_display_sequence_short() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0],
            repeat: Some(true),
        };
        assert_eq!(
            generator_display(&config),
            "sequence ([1.0, 2.0, 3.0], repeat)"
        );
    }

    #[test]
    fn generator_display_sequence_long() {
        let config = GeneratorConfig::Sequence {
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            repeat: Some(false),
        };
        assert_eq!(
            generator_display(&config),
            "sequence ([1, 2, ... 6 total], clamp)"
        );
    }

    #[test]
    fn generator_display_sequence_repeat_none_defaults_to_repeat() {
        let config = GeneratorConfig::Sequence {
            values: vec![10.0],
            repeat: None,
        };
        assert_eq!(generator_display(&config), "sequence ([10.0], repeat)");
    }

    #[test]
    fn generator_display_csv_replay() {
        let config = GeneratorConfig::CsvReplay {
            file: "/data/metrics.csv".to_string(),
            column: Some(2),
            has_header: Some(true),
            repeat: Some(false),
        };
        assert_eq!(
            generator_display(&config),
            "csv_replay (file: /data/metrics.csv, column: 2, header, clamp)"
        );
    }

    #[test]
    fn generator_display_csv_replay_defaults() {
        let config = GeneratorConfig::CsvReplay {
            file: "data.csv".to_string(),
            column: None,
            has_header: None,
            repeat: None,
        };
        assert_eq!(
            generator_display(&config),
            "csv_replay (file: data.csv, column: 0, header, repeat)"
        );
    }

    // -----------------------------------------------------------------------
    // log_generator_display: all LogGeneratorConfig variants
    // -----------------------------------------------------------------------

    #[test]
    fn log_generator_display_template_minimal() {
        let config = LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "test".to_string(),
                field_pools: HashMap::new(),
            }],
            severity_weights: None,
            seed: None,
        };
        assert_eq!(log_generator_display(&config), "template (1 template(s))");
    }

    #[test]
    fn log_generator_display_template_with_seed_and_weights() {
        let mut weights = HashMap::new();
        weights.insert("info".to_string(), 0.7);
        weights.insert("error".to_string(), 0.3);
        let config = LogGeneratorConfig::Template {
            templates: vec![
                TemplateConfig {
                    message: "msg1".to_string(),
                    field_pools: HashMap::new(),
                },
                TemplateConfig {
                    message: "msg2".to_string(),
                    field_pools: HashMap::new(),
                },
            ],
            severity_weights: Some(weights),
            seed: Some(42),
        };
        assert_eq!(
            log_generator_display(&config),
            "template (2 template(s), severity: error=0.3/info=0.7, seed: 42)"
        );
    }

    #[test]
    fn log_generator_display_replay() {
        let config = LogGeneratorConfig::Replay {
            file: "/var/log/app.log".to_string(),
        };
        assert_eq!(
            log_generator_display(&config),
            "replay (file: /var/log/app.log)"
        );
    }

    // -----------------------------------------------------------------------
    // print_start: quiet mode is a no-op (does not panic)
    // -----------------------------------------------------------------------

    /// Helper: build a minimal ScenarioEntry::Metrics for testing.
    fn make_metrics_entry() -> ScenarioEntry {
        ScenarioEntry::Metrics(ScenarioConfig {
            name: "test_metric".to_string(),
            rate: 10.0,
            duration: Some("10s".to_string()),
            generator: GeneratorConfig::Constant { value: 1.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        })
    }

    /// Helper: build a minimal ScenarioEntry::Logs for testing.
    fn make_logs_entry() -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            name: "test_logs".to_string(),
            rate: 5.0,
            duration: Some("5s".to_string()),
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test message".to_string(),
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
        })
    }

    #[test]
    fn print_start_quiet_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        // Should return immediately without writing anything.
        print_start(&entry, Verbosity::Quiet);
    }

    #[test]
    fn print_start_quiet_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Quiet);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        // Output goes to stderr; we just verify no panic.
        print_start(&entry, Verbosity::Normal);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Normal);
    }

    #[test]
    fn print_start_verbose_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        print_start(&entry, Verbosity::Verbose);
    }

    #[test]
    fn print_start_verbose_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Verbose);
    }

    #[test]
    fn print_start_metrics_without_duration_does_not_panic() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "no_dur".to_string(),
            rate: 1.0,
            duration: None,
            generator: GeneratorConfig::Constant { value: 0.0 },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        print_start(&entry, Verbosity::Normal);
    }

    // -----------------------------------------------------------------------
    // print_stop: quiet mode is a no-op (does not panic)
    // -----------------------------------------------------------------------

    #[test]
    fn print_stop_quiet_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("test", Duration::from_secs(5), &stats, Verbosity::Quiet);
    }

    #[test]
    fn print_stop_normal_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("test", Duration::from_secs(5), &stats, Verbosity::Normal);
    }

    #[test]
    fn print_stop_verbose_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("test", Duration::from_secs(5), &stats, Verbosity::Verbose);
    }

    #[test]
    fn print_stop_with_errors_does_not_panic() {
        let stats = ScenarioStats {
            total_events: 100,
            bytes_emitted: 4096,
            errors: 3,
            ..Default::default()
        };
        // When errors > 0, the stop icon should be yellow and error count red.
        // We just verify it does not panic.
        print_stop(
            "error_scenario",
            Duration::from_secs(10),
            &stats,
            Verbosity::Normal,
        );
    }

    #[test]
    fn print_stop_with_zero_duration_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop(
            "zero_dur",
            Duration::from_secs(0),
            &stats,
            Verbosity::Normal,
        );
    }

    #[test]
    fn print_stop_with_large_byte_count_does_not_panic() {
        let stats = ScenarioStats {
            bytes_emitted: 2_000_000_000,
            ..Default::default()
        };
        print_stop(
            "big_bytes",
            Duration::from_secs(60),
            &stats,
            Verbosity::Normal,
        );
    }

    // -----------------------------------------------------------------------
    // print_config: does not panic for metrics and logs entries
    // -----------------------------------------------------------------------

    #[test]
    fn print_config_metrics_does_not_panic() {
        let entry = make_metrics_entry();
        print_config(&entry);
    }

    #[test]
    fn print_config_logs_does_not_panic() {
        let entry = make_logs_entry();
        print_config(&entry);
    }

    #[test]
    fn print_config_metrics_with_all_optional_fields_does_not_panic() {
        use sonda_core::config::{BurstConfig, CardinalitySpikeConfig, GapConfig, SpikeStrategy};

        let mut labels = HashMap::new();
        labels.insert("hostname".to_string(), "web-01".to_string());
        labels.insert("region".to_string(), "us-east-1".to_string());

        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            name: "full_config".to_string(),
            rate: 1000.0,
            duration: Some("30s".to_string()),
            generator: GeneratorConfig::Sine {
                amplitude: 50.0,
                period_secs: 60.0,
                offset: 50.0,
            },
            gaps: Some(GapConfig {
                every: "2m".to_string(),
                r#for: "20s".to_string(),
            }),
            bursts: Some(BurstConfig {
                every: "10s".to_string(),
                r#for: "1s".to_string(),
                multiplier: 5.0,
            }),
            cardinality_spikes: Some(vec![CardinalitySpikeConfig {
                label: "pod_name".to_string(),
                every: "2m".to_string(),
                r#for: "30s".to_string(),
                cardinality: 100,
                strategy: SpikeStrategy::Counter,
                prefix: None,
                seed: None,
            }]),
            labels: Some(labels),
            encoder: EncoderConfig::PrometheusText { precision: Some(2) },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        print_config(&entry);
    }

    #[test]
    fn print_config_logs_with_replay_generator_does_not_panic() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            name: "replay_logs".to_string(),
            rate: 100.0,
            duration: None,
            generator: LogGeneratorConfig::Replay {
                file: "/var/log/app.log".to_string(),
            },
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            encoder: EncoderConfig::Syslog {
                hostname: None,
                app_name: None,
            },
            sink: SinkConfig::File {
                path: "/tmp/out.log".to_string(),
            },
            phase_offset: None,
            clock_group: None,
        });
        print_config(&entry);
    }

    // -----------------------------------------------------------------------
    // print_dry_run_ok: does not panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_dry_run_ok_does_not_panic() {
        print_dry_run_ok();
    }

    // -----------------------------------------------------------------------
    // print_summary: aggregate stats formatting
    // -----------------------------------------------------------------------

    #[test]
    fn print_summary_quiet_mode_does_not_panic() {
        let agg = AggregateStats {
            scenario_count: 3,
            total_events: 150_000,
            total_bytes: 12_000_000,
            total_errors: 0,
        };
        print_summary(&agg, Duration::from_secs_f64(30.2), Verbosity::Quiet);
    }

    #[test]
    fn print_summary_normal_mode_does_not_panic() {
        let agg = AggregateStats {
            scenario_count: 3,
            total_events: 150_000,
            total_bytes: 12_000_000,
            total_errors: 0,
        };
        print_summary(&agg, Duration::from_secs_f64(30.2), Verbosity::Normal);
    }

    #[test]
    fn print_summary_with_errors_does_not_panic() {
        let agg = AggregateStats {
            scenario_count: 2,
            total_events: 50_000,
            total_bytes: 5_000_000,
            total_errors: 5,
        };
        print_summary(&agg, Duration::from_secs(10), Verbosity::Normal);
    }

    #[test]
    fn print_summary_verbose_mode_does_not_panic() {
        let agg = AggregateStats {
            scenario_count: 1,
            total_events: 1000,
            total_bytes: 1024,
            total_errors: 0,
        };
        print_summary(&agg, Duration::from_secs(5), Verbosity::Verbose);
    }

    #[test]
    fn print_summary_zero_scenarios_does_not_panic() {
        let agg = AggregateStats {
            scenario_count: 0,
            total_events: 0,
            total_bytes: 0,
            total_errors: 0,
        };
        print_summary(&agg, Duration::from_secs(0), Verbosity::Normal);
    }
}
