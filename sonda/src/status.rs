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
fn encoder_display(encoder: &EncoderConfig) -> &'static str {
    match encoder {
        EncoderConfig::PrometheusText => "prometheus_text",
        EncoderConfig::InfluxLineProtocol { .. } => "influx_lp",
        EncoderConfig::JsonLines => "json_lines",
        EncoderConfig::Syslog { .. } => "syslog",
        #[cfg(feature = "remote-write")]
        EncoderConfig::RemoteWrite => "remote_write",
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
