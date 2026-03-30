//! Colored lifecycle banners for CLI status output.
//!
//! All output goes to stderr so that stdout remains clean for data (encoded
//! events). The [`print_start`] and [`print_stop`] functions are no-ops when
//! `quiet` is true.

use std::time::Duration;

use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use sonda_core::config::ScenarioEntry;
use sonda_core::encoder::EncoderConfig;
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::SinkConfig;

/// Print a start banner for a scenario to stderr.
///
/// Displays the scenario name, signal type, rate, encoder, sink, and optional
/// duration. Returns immediately if `quiet` is true.
pub fn print_start(entry: &ScenarioEntry, quiet: bool) {
    if quiet {
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
/// errors. The error count is red when non-zero. Returns immediately if `quiet`
/// is true.
pub fn print_stop(name: &str, elapsed: Duration, stats: &ScenarioStats, quiet: bool) {
    if quiet {
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

/// Format a sink config as a human-readable display string.
fn sink_display(sink: &SinkConfig) -> String {
    match sink {
        SinkConfig::Stdout => "stdout".to_string(),
        SinkConfig::File { path } => format!("file: {path}"),
        SinkConfig::Tcp { address } => format!("tcp: {address}"),
        SinkConfig::Udp { address } => format!("udp: {address}"),
        SinkConfig::HttpPush { url, .. } => format!("http: {url}"),
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, .. } => format!("remote_write: {url}"),
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { topic, .. } => format!("kafka: {topic}"),
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

    #[test]
    fn sink_display_loki() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100/loki/api/v1/push".to_string(),
            labels: HashMap::new(),
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
        print_start(&entry, true);
    }

    #[test]
    fn print_start_quiet_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, true);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        // Output goes to stderr; we just verify no panic.
        print_start(&entry, false);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, false);
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
            labels: None,
            encoder: EncoderConfig::PrometheusText { precision: None },
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
        });
        print_start(&entry, false);
    }

    // -----------------------------------------------------------------------
    // print_stop: quiet mode is a no-op (does not panic)
    // -----------------------------------------------------------------------

    #[test]
    fn print_stop_quiet_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("test", Duration::from_secs(5), &stats, true);
    }

    #[test]
    fn print_stop_normal_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("test", Duration::from_secs(5), &stats, false);
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
        print_stop("error_scenario", Duration::from_secs(10), &stats, false);
    }

    #[test]
    fn print_stop_with_zero_duration_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop("zero_dur", Duration::from_secs(0), &stats, false);
    }

    #[test]
    fn print_stop_with_large_byte_count_does_not_panic() {
        let stats = ScenarioStats {
            bytes_emitted: 2_000_000_000,
            ..Default::default()
        };
        print_stop("big_bytes", Duration::from_secs(60), &stats, false);
    }
}
