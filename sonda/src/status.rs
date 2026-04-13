//! Colored lifecycle banners for CLI status output.
//!
//! All output goes to stderr so that stdout remains clean for data (encoded
//! events). The [`print_start`] and [`print_stop`] functions are no-ops when
//! verbosity is [`Verbosity::Quiet`]. The [`print_config`] function displays
//! the resolved scenario config in a human-readable format. The
//! [`print_summary`] function prints an aggregate summary after all scenarios
//! complete in the `run` subcommand. [`print_version`] displays the crate
//! version and enabled features. [`print_show_header`] prints a styled header
//! for the `scenarios show` subcommand. [`print_dry_run_ok`] shows the
//! validation result with a scenario count.
//! hint message for contextual help on errors.

use std::time::Duration;

use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use crate::cli::Verbosity;
use sonda_core::config::{
    BurstConfig, CardinalitySpikeConfig, DynamicLabelConfig, DynamicLabelStrategy, GapConfig,
    HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::SinkConfig;

/// Print a start banner for a scenario to stderr.
///
/// Displays the scenario name, signal type, rate, encoder, sink, and optional
/// duration. When `position` is `Some((index, total))` and `total > 1`, a
/// dimmed `[index/total]` prefix is prepended. Returns immediately if
/// verbosity is [`Verbosity::Quiet`].
pub fn print_start(entry: &ScenarioEntry, verbosity: Verbosity, position: Option<(usize, usize)>) {
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
        ScenarioEntry::Histogram(c) => (
            c.name.as_str(),
            "histogram",
            c.rate,
            encoder_display(&c.encoder),
            sink_display(&c.sink),
            c.duration.as_deref(),
        ),
        ScenarioEntry::Summary(c) => (
            c.name.as_str(),
            "summary",
            c.rate,
            encoder_display(&c.encoder),
            sink_display(&c.sink),
            c.duration.as_deref(),
        ),
    };
    let clock_group = entry.clock_group();

    let arrow = "\u{25b6}".if_supports_color(Stderr, |t| t.green());
    let pos_prefix = format_position_prefix(position);
    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let signal_label = "signal_type:".if_supports_color(Stderr, |t| t.dimmed());
    let rate_label = "rate:".if_supports_color(Stderr, |t| t.dimmed());
    let encoder_label = "encoder:".if_supports_color(Stderr, |t| t.dimmed());
    let sink_label = "sink:".if_supports_color(Stderr, |t| t.dimmed());

    let rate_str = format_rate(rate);
    let rate_per_sec = format!("{rate_str}/s");
    let signal_value = signal_type.if_supports_color(Stderr, |t| t.cyan());
    let rate_value = rate_per_sec.if_supports_color(Stderr, |t| t.cyan());
    let encoder_value = encoder.if_supports_color(Stderr, |t| t.cyan());
    let sink_value = sink.if_supports_color(Stderr, |t| t.cyan());

    // Duration, encoder, sink, signal are always rendered. Duration and
    // clock_group are optional trailing sections; build the line in two
    // steps so both can be omitted cleanly.
    let mut line = format!(
        "{pos_prefix}{arrow} {bold_name}  {signal_label} {signal_value} {pipe} {rate_label} {rate_value} {pipe} {encoder_label} {encoder_value} {pipe} {sink_label} {sink_value}"
    );
    if let Some(dur) = duration {
        let dur_label = "duration:".if_supports_color(Stderr, |t| t.dimmed());
        let dur_value = dur.if_supports_color(Stderr, |t| t.cyan());
        line.push_str(&format!(" {pipe} {dur_label} {dur_value}"));
    }
    if let Some(cg) = clock_group {
        let cg_label = "clock_group:".if_supports_color(Stderr, |t| t.dimmed());
        let cg_display = format_clock_group(cg);
        let cg_value = cg_display.if_supports_color(Stderr, |t| t.cyan());
        line.push_str(&format!(" {pipe} {cg_label} {cg_value}"));
    }
    eprintln!("{line}");
}

/// Render a clock group string for the banner.
///
/// Auto-assigned groups (prefix `chain_`, per the compiler's naming
/// convention) get a trailing ` (auto)` marker so users can distinguish
/// them from explicit assignments. Spec §5 format: `link_failover (auto)`.
pub fn format_clock_group(group: &str) -> String {
    if group.starts_with("chain_") {
        format!("{group} (auto)")
    } else {
        group.to_string()
    }
}

/// Print a stop banner for a scenario to stderr.
///
/// Displays the scenario name, elapsed time, total events, bytes emitted, and
/// error count. The stop icon is colored blue normally, or yellow if there were
/// errors. The error count is red when non-zero. When `position` is
/// `Some((index, total))` and `total > 1`, a dimmed `[index/total]` prefix is
/// prepended. Returns immediately if verbosity is [`Verbosity::Quiet`].
pub fn print_stop(
    name: &str,
    elapsed: Duration,
    stats: &ScenarioStats,
    verbosity: Verbosity,
    position: Option<(usize, usize)>,
) {
    if verbosity == Verbosity::Quiet {
        return;
    }

    let has_errors = stats.errors > 0;

    let square = if has_errors {
        format!("{}", "\u{25a0}".if_supports_color(Stderr, |t| t.yellow()))
    } else {
        format!("{}", "\u{25a0}".if_supports_color(Stderr, |t| t.blue()))
    };

    let pos_prefix = format_position_prefix(position);
    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let elapsed_secs = elapsed.as_secs_f64();
    let elapsed_str = format!("{elapsed_secs:.1}s");
    let bytes_str = format_bytes(stats.bytes_emitted);

    let events_label = "events:".if_supports_color(Stderr, |t| t.dimmed());
    let bytes_label = "bytes:".if_supports_color(Stderr, |t| t.dimmed());
    let errors_label = "errors:".if_supports_color(Stderr, |t| t.dimmed());

    let events_value = if has_errors {
        format!("{}", stats.total_events)
    } else {
        format!(
            "{}",
            stats.total_events.if_supports_color(Stderr, |t| t.green())
        )
    };

    let bytes_value = bytes_str.if_supports_color(Stderr, |t| t.cyan());

    let errors_value = if has_errors {
        format!("{}", stats.errors.if_supports_color(Stderr, |t| t.red()))
    } else {
        format!("{}", stats.errors)
    };

    eprintln!(
        "{pos_prefix}{square} {bold_name}  completed in {elapsed_str} {pipe} {events_label} {events_value} {pipe} {bytes_label} {bytes_value} {pipe} {errors_label} {errors_value}"
    );
}

/// Print the resolved config for a single scenario entry to stderr.
///
/// Used by `--dry-run` (always) and `--verbose` (at startup). The output is
/// human-readable custom formatting, not YAML or Debug output.
///
/// When `index` >= 1 and `total` > 1, a `[index/total]` prefix is shown on the
/// header line and a dimmed separator is printed before each block except the
/// first.
pub fn print_config(entry: &ScenarioEntry, index: usize, total: usize) {
    // Separator between scenario blocks when multiple are shown.
    if total > 1 && index > 1 {
        let sep = "\u{2500}\u{2500}\u{2500}".if_supports_color(Stderr, |t| t.dimmed());
        eprintln!("{sep}");
    }

    match entry {
        ScenarioEntry::Metrics(c) => print_metrics_config(c, index, total),
        ScenarioEntry::Logs(c) => print_logs_config(c, index, total),
        ScenarioEntry::Histogram(c) => print_histogram_config(c, index, total),
        ScenarioEntry::Summary(c) => print_summary_config(c, index, total),
    }
}

/// Print the resolved config for a metrics scenario.
fn print_metrics_config(c: &ScenarioConfig, index: usize, total: usize) {
    print_config_header(&c.name, index, total);
    eprintln!();
    print_config_field("name:", &c.name);
    print_config_field("signal:", "metrics");
    print_config_field("rate:", &format!("{}/s", format_rate(c.rate)));
    print_config_field("duration:", c.duration.as_deref().unwrap_or("indefinite"));
    print_config_field("generator:", &generator_display(&c.generator));
    print_config_field("encoder:", &encoder_display(&c.encoder));
    print_config_field("sink:", &sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    print_dynamic_labels_lines(&c.dynamic_labels);
    print_jitter_line(&c.jitter, &c.jitter_seed);
    print_phase_offset_line(&c.phase_offset);
    print_clock_group_line(&c.clock_group);
    eprintln!();
}

/// Print the resolved config for a logs scenario.
fn print_logs_config(c: &LogScenarioConfig, index: usize, total: usize) {
    print_config_header(&c.name, index, total);
    eprintln!();
    print_config_field("name:", &c.name);
    print_config_field("signal:", "logs");
    print_config_field("rate:", &format!("{}/s", format_rate(c.rate)));
    print_config_field("duration:", c.duration.as_deref().unwrap_or("indefinite"));
    print_config_field("generator:", &log_generator_display(&c.generator));
    print_config_field("encoder:", &encoder_display(&c.encoder));
    print_config_field("sink:", &sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    print_dynamic_labels_lines(&c.dynamic_labels);
    print_jitter_line(&c.jitter, &c.jitter_seed);
    print_phase_offset_line(&c.phase_offset);
    print_clock_group_line(&c.clock_group);
    eprintln!();
}

/// Print the resolved config for a histogram scenario.
fn print_histogram_config(c: &HistogramScenarioConfig, index: usize, total: usize) {
    print_config_header(&c.name, index, total);
    eprintln!();
    print_config_field("name:", &c.name);
    print_config_field("signal:", "histogram");
    print_config_field("rate:", &format!("{}/s", format_rate(c.rate)));
    print_config_field("duration:", c.duration.as_deref().unwrap_or("indefinite"));
    print_config_field(
        "buckets:",
        &match &c.buckets {
            Some(b) => format!("{:?}", b),
            None => "default (Prometheus)".to_string(),
        },
    );
    print_config_field("distribution:", &format!("{:?}", c.distribution));
    print_config_field(
        "obs/tick:",
        &format!("{}", c.observations_per_tick.unwrap_or(100)),
    );
    print_config_field("encoder:", &encoder_display(&c.encoder));
    print_config_field("sink:", &sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    print_dynamic_labels_lines(&c.dynamic_labels);
    print_jitter_line(&c.jitter, &c.jitter_seed);
    print_phase_offset_line(&c.phase_offset);
    print_clock_group_line(&c.clock_group);
    eprintln!();
}

/// Print the resolved config for a summary scenario.
fn print_summary_config(c: &SummaryScenarioConfig, index: usize, total: usize) {
    print_config_header(&c.name, index, total);
    eprintln!();
    print_config_field("name:", &c.name);
    print_config_field("signal:", "summary");
    print_config_field("rate:", &format!("{}/s", format_rate(c.rate)));
    print_config_field("duration:", c.duration.as_deref().unwrap_or("indefinite"));
    print_config_field(
        "quantiles:",
        &match &c.quantiles {
            Some(q) => format!("{:?}", q),
            None => "default [0.5, 0.9, 0.95, 0.99]".to_string(),
        },
    );
    print_config_field("distribution:", &format!("{:?}", c.distribution));
    print_config_field(
        "obs/tick:",
        &format!("{}", c.observations_per_tick.unwrap_or(100)),
    );
    print_config_field("encoder:", &encoder_display(&c.encoder));
    print_config_field("sink:", &sink_display(&c.sink));
    print_labels_line(&c.labels);
    print_gaps_line(&c.gaps);
    print_bursts_line(&c.bursts);
    print_spikes_lines(&c.cardinality_spikes);
    print_dynamic_labels_lines(&c.dynamic_labels);
    print_jitter_line(&c.jitter, &c.jitter_seed);
    print_phase_offset_line(&c.phase_offset);
    print_clock_group_line(&c.clock_group);
    eprintln!();
}

/// Format a `[index/total]` position prefix for multi-scenario banners.
///
/// Returns a dimmed `[1/5] ` string when `total > 1`, or an empty string
/// when there is only one scenario (or no position info).
fn format_position_prefix(position: Option<(usize, usize)>) -> String {
    match position {
        Some((index, total)) if total > 1 => {
            let tag = format!("[{index}/{total}]");
            format!("{} ", tag.if_supports_color(Stderr, |t| t.dimmed()))
        }
        _ => String::new(),
    }
}

/// Print the `[config]` header line for a scenario config block.
///
/// When `total > 1`, a numbering prefix is included: `[config] [1/5] name`.
/// When `total == 1`, the format is `[config] name`.
fn print_config_header(name: &str, index: usize, total: usize) {
    let header = "[config]".if_supports_color(Stderr, |t| t.cyan());
    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    if total > 1 {
        let numbering = format!("[{index}/{total}]");
        let numbering = numbering.if_supports_color(Stderr, |t| t.dimmed());
        eprintln!("{header} {numbering} {bold_name}");
    } else {
        eprintln!("{header} {bold_name}");
    }
}

/// Print a single config field line: bold label, cyan value.
fn print_config_field(field_label: &str, value: &str) {
    let label = format!("{:<14}", field_label);
    let label = label.if_supports_color(Stderr, |t| t.bold());
    let colored_value = value.if_supports_color(Stderr, |t| t.cyan());
    eprintln!("  {label} {colored_value}");
}

/// Print the labels line if labels are present and non-empty.
fn print_labels_line(labels: &Option<std::collections::HashMap<String, String>>) {
    if let Some(ref map) = labels {
        if !map.is_empty() {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
            let formatted: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let label = format!("{:<14}", "labels:");
            let label = label.if_supports_color(Stderr, |t| t.bold());
            eprintln!("  {label} {}", formatted.join(", "));
        }
    }
}

/// Print the gaps line if gap config is present.
fn print_gaps_line(gaps: &Option<GapConfig>) {
    if let Some(ref g) = gaps {
        let label = format!("{:<14}", "gaps:");
        let label = label.if_supports_color(Stderr, |t| t.bold());
        eprintln!("  {label} every {}, for {}", g.every, g.r#for);
    }
}

/// Print the bursts line if burst config is present.
fn print_bursts_line(bursts: &Option<BurstConfig>) {
    if let Some(ref b) = bursts {
        let label = format!("{:<14}", "bursts:");
        let label = label.if_supports_color(Stderr, |t| t.bold());
        eprintln!(
            "  {label} every {}, for {}, multiplier {}x",
            b.every, b.r#for, b.multiplier
        );
    }
}

/// Print cardinality spike lines if spikes are configured.
fn print_spikes_lines(spikes: &Option<Vec<CardinalitySpikeConfig>>) {
    if let Some(ref list) = spikes {
        for s in list {
            let label = format!("{:<14}", "spikes:");
            let label = label.if_supports_color(Stderr, |t| t.bold());
            eprintln!(
                "  {label} label={}, every {}, for {}, cardinality={}",
                s.label, s.every, s.r#for, s.cardinality
            );
        }
    }
}

/// Print dynamic label lines if dynamic labels are configured.
fn print_dynamic_labels_lines(dynamic_labels: &Option<Vec<DynamicLabelConfig>>) {
    if let Some(ref list) = dynamic_labels {
        for dl in list {
            let label = format!("{:<14}", "dynamic:");
            let label = label.if_supports_color(Stderr, |t| t.bold());
            match &dl.strategy {
                DynamicLabelStrategy::Counter {
                    prefix,
                    cardinality,
                } => {
                    let pfx = prefix.as_deref().unwrap_or("");
                    eprintln!(
                        "  {label} key={}, counter (prefix={:?}, cardinality={})",
                        dl.key, pfx, cardinality
                    );
                }
                DynamicLabelStrategy::ValuesList { values } => {
                    if values.len() <= 5 {
                        eprintln!("  {label} key={}, values {:?}", dl.key, values);
                    } else {
                        eprintln!(
                            "  {label} key={}, values [{}, {}, ... {} total]",
                            dl.key,
                            values[0],
                            values[1],
                            values.len()
                        );
                    }
                }
            }
        }
    }
}

/// Print the jitter line if jitter is configured.
fn print_jitter_line(jitter: &Option<f64>, jitter_seed: &Option<u64>) {
    if let Some(j) = jitter {
        let seed_str = jitter_seed
            .map(|s| format!(", seed: {s}"))
            .unwrap_or_default();
        let label = format!("{:<14}", "jitter:");
        let label = label.if_supports_color(Stderr, |t| t.bold());
        eprintln!("  {label} +/-{j}{seed_str}");
    }
}

/// Print the phase_offset line if set.
fn print_phase_offset_line(phase_offset: &Option<String>) {
    if let Some(ref offset) = phase_offset {
        let label = format!("{:<14}", "phase_offset:");
        let label = label.if_supports_color(Stderr, |t| t.bold());
        eprintln!("  {label} {offset}");
    }
}

/// Print the clock_group line if set.
fn print_clock_group_line(clock_group: &Option<String>) {
    if let Some(ref group) = clock_group {
        let label = format!("{:<14}", "clock_group:");
        let label = label.if_supports_color(Stderr, |t| t.bold());
        eprintln!("  {label} {group}");
    }
}

/// Print the dry-run validation result to stderr.
///
/// Called after all entries are printed in dry-run mode to confirm that
/// validation passed. The `scenario_count` is displayed alongside the OK
/// status (e.g. `"Validation: OK (3 scenarios)"`).
pub fn print_dry_run_ok(scenario_count: usize) {
    let ok_label = "OK".if_supports_color(Stderr, |t| t.green());
    let noun = if scenario_count == 1 {
        "scenario"
    } else {
        "scenarios"
    };
    eprintln!("Validation: {ok_label} ({scenario_count} {noun})");
}

/// Build the version string displayed by [`print_version`].
///
/// Returns a line like `"sonda 0.10.0"` or `"sonda 0.10.0 (http, kafka)"`
/// depending on which optional features are compiled in. Exposed for testing.
pub fn version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut features: Vec<&str> = Vec::new();

    if cfg!(feature = "http") {
        features.push("http");
    }
    if cfg!(feature = "remote-write") {
        features.push("remote-write");
    }
    if cfg!(feature = "kafka") {
        features.push("kafka");
    }
    if cfg!(feature = "otlp") {
        features.push("otlp");
    }

    if features.is_empty() {
        format!("sonda {version}")
    } else {
        format!("sonda {version} ({})", features.join(", "))
    }
}

/// Print a styled product header to stderr.
///
/// Displays the crate name in bold cyan, version in plain text, and a dimmed
/// tagline: `sonda 0.11.0 -- synthetic telemetry generator`. Called when
/// verbosity is [`Verbosity::Verbose`], before printing the config.
///
/// The plain-text version string (from [`version_string`]) is embedded in
/// the styled output so that integration tests can assert on
/// `"sonda {version}"`.
pub fn print_version() {
    let vs = version_string();
    let version = env!("CARGO_PKG_VERSION");
    let style = owo_colors::Style::new().bold().cyan();
    let name = "sonda".if_supports_color(Stderr, |t| t.style(style));
    let dash = "\u{2014}".if_supports_color(Stderr, |t| t.dimmed());
    let tagline = "synthetic telemetry generator".if_supports_color(Stderr, |t| t.dimmed());

    // If features are enabled, show them after the version.
    let prefix_len = format!("sonda {version}").len();
    let features_suffix = if vs.len() > prefix_len {
        // Extract the features part from version_string (e.g. " (http, kafka)")
        &vs[prefix_len..]
    } else {
        ""
    };

    eprintln!("{name} {version}{features_suffix} {dash} {tagline}");
}

/// Print a styled header line for the `scenarios show` subcommand to stderr.
///
/// Displays the scenario name, category, and signal type in a format
/// consistent with the start banner styling. The YAML content itself is
/// printed separately to stdout.
pub fn print_show_header(name: &str, category: &str, signal_type: &str) {
    let name_label = "scenario:".if_supports_color(Stderr, |t| t.dimmed());
    let bold_name = name.if_supports_color(Stderr, |t| t.bold());
    let cat_label = "category:".if_supports_color(Stderr, |t| t.dimmed());
    let sig_label = "signal:".if_supports_color(Stderr, |t| t.dimmed());
    eprintln!("{name_label} {bold_name}  {cat_label} {category}  {sig_label} {signal_type}");
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

/// Per-clock-group aggregate row for the clock-group-aware summary.
///
/// One [`ClockGroupStats`] per distinct `clock_group` observed across
/// launched scenarios. `group` is `None` for scenarios with no clock
/// group assignment — those get a synthetic "ungrouped" bucket.
pub struct ClockGroupStats {
    /// Clock group name, or `None` for scenarios without one.
    pub group: Option<String>,
    /// Number of scenarios in this group.
    pub scenario_count: usize,
    /// Total events across scenarios in this group.
    pub total_events: u64,
    /// Total bytes emitted across scenarios in this group.
    pub total_bytes: u64,
    /// Total errors across scenarios in this group.
    pub total_errors: u64,
}

/// Print an aggregate summary grouped by `clock_group` to stderr.
///
/// Emits one line per group (in the order supplied by the caller, which
/// is deterministic source order from the compiler), followed by the
/// cross-group totals from [`print_summary`]. Returns immediately if
/// verbosity is [`Verbosity::Quiet`] or if no groups are provided.
///
/// The caller is responsible for deciding when to invoke this vs. the
/// flat summary — the convention is "2+ distinct groups, at least one
/// of which is non-None".
pub fn print_summary_by_clock_group(
    groups: &[ClockGroupStats],
    total: &AggregateStats,
    total_elapsed: Duration,
    verbosity: Verbosity,
) {
    if verbosity == Verbosity::Quiet || groups.is_empty() {
        return;
    }

    let header_bar = "\u{2501}\u{2501}".if_supports_color(Stderr, |t| t.bold());
    let header_label = "run complete (by clock_group)".if_supports_color(Stderr, |t| t.bold());
    eprintln!("{header_bar} {header_label}");

    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    for g in groups {
        let group_label = match g.group {
            Some(ref name) => format_clock_group(name),
            None => "(ungrouped)".to_string(),
        };
        let bold_group = group_label.if_supports_color(Stderr, |t| t.bold());
        let has_errors = g.total_errors > 0;
        let scenarios_value = format!("{}", g.scenario_count);
        let events_value = if has_errors {
            format!("{}", g.total_events)
        } else {
            format!(
                "{}",
                g.total_events.if_supports_color(Stderr, |t| t.green())
            )
        };
        let bytes_str = format_bytes(g.total_bytes);
        let bytes_value = bytes_str.if_supports_color(Stderr, |t| t.cyan());
        let errors_value = if has_errors {
            format!("{}", g.total_errors.if_supports_color(Stderr, |t| t.red()))
        } else {
            format!("{}", g.total_errors)
        };
        eprintln!(
            "  {bold_group}  scenarios: {scenarios_value} {pipe} events: {events_value} {pipe} bytes: {bytes_value} {pipe} errors: {errors_value}"
        );
    }

    print_summary(total, total_elapsed, verbosity);
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

    let has_errors = agg.total_errors > 0;

    let bar = "\u{2501}\u{2501}".if_supports_color(Stderr, |t| t.bold());
    let label = "run complete".if_supports_color(Stderr, |t| t.bold());
    let pipe = "|".if_supports_color(Stderr, |t| t.dimmed());
    let elapsed_str = format!("{:.1}s", total_elapsed.as_secs_f64());
    let bytes_str = format_bytes(agg.total_bytes);

    let scenarios_value = format!(
        "{}",
        agg.scenario_count.if_supports_color(Stderr, |t| t.bold())
    );

    let events_value = if has_errors {
        format!("{}", agg.total_events)
    } else {
        format!(
            "{}",
            agg.total_events.if_supports_color(Stderr, |t| t.green())
        )
    };

    let bytes_value = bytes_str.if_supports_color(Stderr, |t| t.cyan());

    let errors_value = if has_errors {
        format!(
            "{}",
            agg.total_errors.if_supports_color(Stderr, |t| t.red())
        )
    } else {
        format!("{}", agg.total_errors)
    };

    eprintln!(
        "{bar} {label}  scenarios: {scenarios_value} {pipe} events: {events_value} {pipe} bytes: {bytes_value} {pipe} errors: {errors_value} {pipe} elapsed: {elapsed_str}"
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
        GeneratorConfig::Spike {
            baseline,
            magnitude,
            duration_secs,
            interval_secs,
        } => format!(
            "spike (baseline: {baseline}, magnitude: {magnitude}, duration: {duration_secs}s, interval: {interval_secs}s)"
        ),
        GeneratorConfig::CsvReplay {
            file,
            column,
            repeat,
            columns,
        } => {
            let rpt = if repeat.unwrap_or(true) {
                "repeat"
            } else {
                "clamp"
            };
            if let Some(ref cols) = columns {
                let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
                format!(
                    "csv_replay (file: {file}, columns: [{}], {rpt})",
                    names.join(", ")
                )
            } else if let Some(col) = column {
                format!("csv_replay (file: {file}, column: {col}, {rpt})")
            } else {
                format!("csv_replay (file: {file}, auto, {rpt})")
            }
        }
        GeneratorConfig::Step {
            start,
            step_size,
            max,
        } => {
            let start_val = start.unwrap_or(0.0);
            let max_str = max.map(|m| format!(", max: {m}")).unwrap_or_default();
            format!("step (start: {start_val}, step: {step_size}{max_str})")
        }
        // Operational aliases — these should be desugared before display in
        // normal operation, but we handle them for completeness.
        GeneratorConfig::Flap { up_duration, down_duration, .. } => {
            let up = up_duration.as_deref().unwrap_or("10s");
            let down = down_duration.as_deref().unwrap_or("5s");
            format!("flap (up: {up}, down: {down})")
        }
        GeneratorConfig::Saturation { baseline, ceiling, time_to_saturate } => {
            let base = baseline.unwrap_or(0.0);
            let ceil = ceiling.unwrap_or(100.0);
            let dur = time_to_saturate.as_deref().unwrap_or("5m");
            format!("saturation (baseline: {base}, ceiling: {ceil}, period: {dur})")
        }
        GeneratorConfig::Leak { baseline, ceiling, time_to_ceiling } => {
            let base = baseline.unwrap_or(0.0);
            let ceil = ceiling.unwrap_or(100.0);
            let dur = time_to_ceiling.as_deref().unwrap_or("10m");
            format!("leak (baseline: {base}, ceiling: {ceil}, time: {dur})")
        }
        GeneratorConfig::Degradation { baseline, ceiling, time_to_degrade, noise, .. } => {
            let base = baseline.unwrap_or(0.0);
            let ceil = ceiling.unwrap_or(100.0);
            let dur = time_to_degrade.as_deref().unwrap_or("5m");
            let n = noise.unwrap_or(1.0);
            format!("degradation (baseline: {base}, ceiling: {ceil}, time: {dur}, noise: {n})")
        }
        GeneratorConfig::Steady { center, amplitude, period, noise, .. } => {
            let c = center.unwrap_or(50.0);
            let a = amplitude.unwrap_or(10.0);
            let p = period.as_deref().unwrap_or("60s");
            let n = noise.unwrap_or(1.0);
            format!("steady (center: {c}, amplitude: {a}, period: {p}, noise: {n})")
        }
        GeneratorConfig::SpikeEvent { baseline, spike_height, spike_duration, spike_interval } => {
            let base = baseline.unwrap_or(0.0);
            let h = spike_height.unwrap_or(100.0);
            let d = spike_duration.as_deref().unwrap_or("10s");
            let i = spike_interval.as_deref().unwrap_or("30s");
            format!("spike_event (baseline: {base}, height: {h}, duration: {d}, interval: {i})")
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
                pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
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
        SinkConfig::Tcp { address, .. } => format!("tcp: {address}"),
        SinkConfig::Udp { address } => format!("udp: {address}"),
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { url, .. } => format!("http: {url}"),
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, .. } => format!("remote_write: {url}"),
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { topic, .. } => format!("kafka: {topic}"),
        #[cfg(feature = "http")]
        SinkConfig::Loki { url, .. } => format!("loki: {url}"),
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc { endpoint, .. } => format!("otlp_grpc: {endpoint}"),
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled { .. } => "http_push (feature disabled)".to_string(),
        #[cfg(not(feature = "http"))]
        SinkConfig::LokiDisabled { .. } => "loki (feature disabled)".to_string(),
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled { .. } => "remote_write (feature disabled)".to_string(),
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled { .. } => "kafka (feature disabled)".to_string(),
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled { .. } => "otlp_grpc (feature disabled)".to_string(),
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
        #[cfg(feature = "otlp")]
        EncoderConfig::Otlp => ("otlp", None),
        #[cfg(not(feature = "remote-write"))]
        EncoderConfig::RemoteWriteDisabled { .. } => ("remote_write (feature disabled)", None),
        #[cfg(not(feature = "otlp"))]
        EncoderConfig::OtlpDisabled { .. } => ("otlp (feature disabled)", None),
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

    use std::collections::{BTreeMap, HashMap};
    use std::time::Duration;

    use sonda_core::config::{BaseScheduleConfig, LogScenarioConfig, ScenarioConfig};
    use sonda_core::encoder::EncoderConfig;
    use sonda_core::generator::{
        CsvColumnSpec, GeneratorConfig, LogGeneratorConfig, TemplateConfig,
    };
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
            retry: None,
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
            retry: None,
        };
        assert_eq!(sink_display(&config), "http: http://localhost:9090/write");
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_display_loki() {
        let config = SinkConfig::Loki {
            url: "http://localhost:3100/loki/api/v1/push".to_string(),
            batch_size: None,
            retry: None,
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
            retry: None,
            tls: None,
            sasl: None,
        };
        assert_eq!(sink_display(&config), "kafka: sonda-events");
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn sink_display_otlp_grpc() {
        use sonda_core::sink::otlp_grpc::OtlpSignalType;
        let config = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: OtlpSignalType::Metrics,
            batch_size: None,
        };
        assert_eq!(sink_display(&config), "otlp_grpc: http://localhost:4317");
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

    #[cfg(feature = "otlp")]
    #[test]
    fn encoder_display_otlp() {
        assert_eq!(encoder_display(&EncoderConfig::Otlp), "otlp");
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
    fn generator_display_spike() {
        let config = GeneratorConfig::Spike {
            baseline: 50.0,
            magnitude: 200.0,
            duration_secs: 10.0,
            interval_secs: 60.0,
        };
        assert_eq!(
            generator_display(&config),
            "spike (baseline: 50, magnitude: 200, duration: 10s, interval: 60s)"
        );
    }

    #[test]
    fn generator_display_csv_replay_with_column() {
        let config = GeneratorConfig::CsvReplay {
            file: "/data/metrics.csv".to_string(),
            column: Some(2),
            repeat: Some(false),
            columns: None,
        };
        assert_eq!(
            generator_display(&config),
            "csv_replay (file: /data/metrics.csv, column: 2, clamp)"
        );
    }

    #[test]
    fn generator_display_csv_replay_auto() {
        let config = GeneratorConfig::CsvReplay {
            file: "data.csv".to_string(),
            column: None,
            repeat: None,
            columns: None,
        };
        assert_eq!(
            generator_display(&config),
            "csv_replay (file: data.csv, auto, repeat)"
        );
    }

    #[test]
    fn generator_display_csv_replay_with_columns() {
        let config = GeneratorConfig::CsvReplay {
            file: "/data/metrics.csv".to_string(),
            column: None,
            repeat: Some(false),
            columns: Some(vec![
                CsvColumnSpec {
                    index: 1,
                    name: "cpu_percent".to_string(),
                    labels: None,
                },
                CsvColumnSpec {
                    index: 2,
                    name: "mem_percent".to_string(),
                    labels: None,
                },
            ]),
        };
        assert_eq!(
            generator_display(&config),
            "csv_replay (file: /data/metrics.csv, columns: [cpu_percent, mem_percent], clamp)"
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
                field_pools: BTreeMap::new(),
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
                    field_pools: BTreeMap::new(),
                },
                TemplateConfig {
                    message: "msg2".to_string(),
                    field_pools: BTreeMap::new(),
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
            base: BaseScheduleConfig {
                name: "test_metric".to_string(),
                rate: 10.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        })
    }

    /// Helper: build a minimal ScenarioEntry::Logs for testing.
    fn make_logs_entry() -> ScenarioEntry {
        ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "test_logs".to_string(),
                rate: 5.0,
                duration: Some("5s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test message".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        })
    }

    #[test]
    fn print_start_quiet_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        // Should return immediately without writing anything.
        print_start(&entry, Verbosity::Quiet, None);
    }

    #[test]
    fn print_start_quiet_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Quiet, None);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        // Output goes to stderr; we just verify no panic.
        print_start(&entry, Verbosity::Normal, None);
    }

    #[test]
    fn print_start_normal_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Normal, None);
    }

    #[test]
    fn print_start_verbose_mode_does_not_panic_for_metrics() {
        let entry = make_metrics_entry();
        print_start(&entry, Verbosity::Verbose, None);
    }

    #[test]
    fn print_start_verbose_mode_does_not_panic_for_logs() {
        let entry = make_logs_entry();
        print_start(&entry, Verbosity::Verbose, None);
    }

    #[test]
    fn print_start_metrics_without_duration_does_not_panic() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "no_dur".to_string(),
                rate: 1.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 0.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        print_start(&entry, Verbosity::Normal, None);
    }

    // -----------------------------------------------------------------------
    // print_stop: quiet mode is a no-op (does not panic)
    // -----------------------------------------------------------------------

    #[test]
    fn print_stop_quiet_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop(
            "test",
            Duration::from_secs(5),
            &stats,
            Verbosity::Quiet,
            None,
        );
    }

    #[test]
    fn print_stop_normal_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop(
            "test",
            Duration::from_secs(5),
            &stats,
            Verbosity::Normal,
            None,
        );
    }

    #[test]
    fn print_stop_verbose_mode_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop(
            "test",
            Duration::from_secs(5),
            &stats,
            Verbosity::Verbose,
            None,
        );
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
            None,
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
            None,
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
            None,
        );
    }

    // -----------------------------------------------------------------------
    // print_config: does not panic for metrics and logs entries
    // -----------------------------------------------------------------------

    #[test]
    fn print_config_metrics_does_not_panic() {
        let entry = make_metrics_entry();
        print_config(&entry, 1, 1);
    }

    #[test]
    fn print_config_logs_does_not_panic() {
        let entry = make_logs_entry();
        print_config(&entry, 1, 1);
    }

    #[test]
    fn print_config_metrics_with_all_optional_fields_does_not_panic() {
        use sonda_core::config::{
            BurstConfig, CardinalitySpikeConfig, DynamicLabelConfig, DynamicLabelStrategy,
            GapConfig, SpikeStrategy,
        };

        let mut labels = HashMap::new();
        labels.insert("hostname".to_string(), "web-01".to_string());
        labels.insert("region".to_string(), "us-east-1".to_string());

        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "full_config".to_string(),
                rate: 1000.0,
                duration: Some("30s".to_string()),
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
                dynamic_labels: Some(vec![DynamicLabelConfig {
                    key: "instance".to_string(),
                    strategy: DynamicLabelStrategy::Counter {
                        prefix: Some("node-".to_string()),
                        cardinality: 5,
                    },
                }]),
                labels: Some(labels),
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Sine {
                amplitude: 50.0,
                period_secs: 60.0,
                offset: 50.0,
            },
            encoder: EncoderConfig::PrometheusText { precision: Some(2) },
        });
        print_config(&entry, 1, 1);
    }

    #[test]
    fn print_config_logs_with_replay_generator_does_not_panic() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "replay_logs".to_string(),
                rate: 100.0,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::File {
                    path: "/tmp/out.log".to_string(),
                },
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Replay {
                file: "/var/log/app.log".to_string(),
            },
            encoder: EncoderConfig::Syslog {
                hostname: None,
                app_name: None,
            },
        });
        print_config(&entry, 1, 1);
    }

    // -----------------------------------------------------------------------
    // print_dry_run_ok: does not panic, correct pluralization
    // -----------------------------------------------------------------------

    #[test]
    fn print_dry_run_ok_single_scenario_does_not_panic() {
        print_dry_run_ok(1);
    }

    #[test]
    fn print_dry_run_ok_multiple_scenarios_does_not_panic() {
        print_dry_run_ok(3);
    }

    #[test]
    fn print_dry_run_ok_zero_scenarios_does_not_panic() {
        print_dry_run_ok(0);
    }

    // -----------------------------------------------------------------------
    // version_string / print_version: content and no-panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_version_does_not_panic() {
        print_version();
    }

    #[test]
    fn version_string_contains_cargo_pkg_version() {
        let vs = version_string();
        let expected_version = env!("CARGO_PKG_VERSION");
        assert!(
            vs.contains(expected_version),
            "version_string() must contain CARGO_PKG_VERSION ({expected_version}), got: {vs}"
        );
    }

    #[test]
    fn version_string_starts_with_sonda_prefix() {
        let vs = version_string();
        assert!(
            vs.starts_with("sonda "),
            "version_string() must start with 'sonda ', got: {vs}"
        );
    }

    // -----------------------------------------------------------------------
    // print_show_header: does not panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_show_header_does_not_panic() {
        print_show_header("cpu-spike", "infrastructure", "metrics");
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

    // -----------------------------------------------------------------------
    // print_phase_offset_line / print_clock_group_line: no panic, correct skip
    // -----------------------------------------------------------------------

    #[test]
    fn print_phase_offset_line_with_value_does_not_panic() {
        print_phase_offset_line(&Some("5s".to_string()));
    }

    #[test]
    fn print_phase_offset_line_with_none_does_not_panic() {
        print_phase_offset_line(&None);
    }

    #[test]
    fn print_clock_group_line_with_value_does_not_panic() {
        print_clock_group_line(&Some("alert-test".to_string()));
    }

    #[test]
    fn print_clock_group_line_with_none_does_not_panic() {
        print_clock_group_line(&None);
    }

    #[test]
    fn print_config_metrics_with_phase_offset_and_clock_group_does_not_panic() {
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "correlated_metric".to_string(),
                rate: 10.0,
                duration: Some("30s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("5s".to_string()),
                clock_group: Some("alert-group".to_string()),
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        print_config(&entry, 1, 1);
    }

    #[test]
    fn print_config_logs_with_phase_offset_and_clock_group_does_not_panic() {
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "correlated_logs".to_string(),
                rate: 5.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: Some("2s".to_string()),
                clock_group: Some("log-sync".to_string()),
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: None,
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        print_config(&entry, 1, 1);
    }

    // -----------------------------------------------------------------------
    // print_dynamic_labels_lines: no panic, correct skip for None/empty
    // -----------------------------------------------------------------------

    #[test]
    fn print_dynamic_labels_lines_with_none_does_not_panic() {
        print_dynamic_labels_lines(&None);
    }

    #[test]
    fn print_dynamic_labels_lines_with_empty_vec_does_not_panic() {
        print_dynamic_labels_lines(&Some(vec![]));
    }

    #[test]
    fn print_dynamic_labels_lines_counter_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        print_dynamic_labels_lines(&Some(vec![DynamicLabelConfig {
            key: "hostname".to_string(),
            strategy: DynamicLabelStrategy::Counter {
                prefix: Some("host-".to_string()),
                cardinality: 10,
            },
        }]));
    }

    #[test]
    fn print_dynamic_labels_lines_counter_without_prefix_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        print_dynamic_labels_lines(&Some(vec![DynamicLabelConfig {
            key: "id".to_string(),
            strategy: DynamicLabelStrategy::Counter {
                prefix: None,
                cardinality: 5,
            },
        }]));
    }

    #[test]
    fn print_dynamic_labels_lines_values_list_short_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        print_dynamic_labels_lines(&Some(vec![DynamicLabelConfig {
            key: "region".to_string(),
            strategy: DynamicLabelStrategy::ValuesList {
                values: vec![
                    "us-east-1".to_string(),
                    "us-west-2".to_string(),
                    "eu-west-1".to_string(),
                ],
            },
        }]));
    }

    #[test]
    fn print_dynamic_labels_lines_values_list_long_truncates() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        print_dynamic_labels_lines(&Some(vec![DynamicLabelConfig {
            key: "zone".to_string(),
            strategy: DynamicLabelStrategy::ValuesList {
                values: vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                    "d".to_string(),
                    "e".to_string(),
                    "f".to_string(),
                ],
            },
        }]));
    }

    #[test]
    fn print_dynamic_labels_lines_multiple_entries_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        print_dynamic_labels_lines(&Some(vec![
            DynamicLabelConfig {
                key: "hostname".to_string(),
                strategy: DynamicLabelStrategy::Counter {
                    prefix: Some("web-".to_string()),
                    cardinality: 3,
                },
            },
            DynamicLabelConfig {
                key: "region".to_string(),
                strategy: DynamicLabelStrategy::ValuesList {
                    values: vec!["us-east-1".to_string(), "eu-west-1".to_string()],
                },
            },
        ]));
    }

    #[test]
    fn print_config_metrics_with_dynamic_labels_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        let entry = ScenarioEntry::Metrics(ScenarioConfig {
            base: BaseScheduleConfig {
                name: "dyn_labels_metric".to_string(),
                rate: 10.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: Some(vec![DynamicLabelConfig {
                    key: "hostname".to_string(),
                    strategy: DynamicLabelStrategy::Counter {
                        prefix: Some("host-".to_string()),
                        cardinality: 10,
                    },
                }]),
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::PrometheusText { precision: None },
        });
        print_config(&entry, 1, 1);
    }

    #[test]
    fn print_config_logs_with_dynamic_labels_does_not_panic() {
        use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy};
        let entry = ScenarioEntry::Logs(LogScenarioConfig {
            base: BaseScheduleConfig {
                name: "dyn_labels_logs".to_string(),
                rate: 5.0,
                duration: Some("10s".to_string()),
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: Some(vec![DynamicLabelConfig {
                    key: "pod_name".to_string(),
                    strategy: DynamicLabelStrategy::Counter {
                        prefix: Some("api-".to_string()),
                        cardinality: 3,
                    },
                }]),
                labels: None,
                sink: SinkConfig::Stdout,
                phase_offset: None,
                clock_group: None,
                jitter: None,
                jitter_seed: None,
            },
            generator: LogGeneratorConfig::Template {
                templates: vec![TemplateConfig {
                    message: "test".to_string(),
                    field_pools: BTreeMap::new(),
                }],
                severity_weights: None,
                seed: Some(0),
            },
            encoder: EncoderConfig::JsonLines { precision: None },
        });
        print_config(&entry, 1, 1);
    }

    // -----------------------------------------------------------------------
    // format_position_prefix: output depends on total count
    // -----------------------------------------------------------------------

    #[test]
    fn format_position_prefix_none_returns_empty() {
        assert_eq!(format_position_prefix(None), "");
    }

    #[test]
    fn format_position_prefix_single_scenario_returns_empty() {
        assert_eq!(format_position_prefix(Some((1, 1))), "");
    }

    #[test]
    fn format_position_prefix_multi_scenario_contains_index() {
        let result = format_position_prefix(Some((2, 5)));
        // The raw string should contain [2/5] regardless of ANSI coloring.
        assert!(
            result.contains("[2/5]"),
            "position prefix for 2/5 must contain [2/5], got: {result:?}"
        );
    }

    #[test]
    fn format_position_prefix_multi_scenario_ends_with_space() {
        let result = format_position_prefix(Some((1, 3)));
        assert!(
            result.ends_with(' '),
            "position prefix must end with a space, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // print_config: multi-scenario numbering does not panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_config_multi_scenario_numbering_does_not_panic() {
        let entry = make_metrics_entry();
        // Simulate second of three scenarios.
        print_config(&entry, 2, 3);
    }

    #[test]
    fn print_config_first_of_multi_does_not_print_separator() {
        let entry = make_metrics_entry();
        // First entry: no separator expected (no panic).
        print_config(&entry, 1, 5);
    }

    // -----------------------------------------------------------------------
    // print_start / print_stop with position: does not panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_start_with_position_does_not_panic() {
        let entry = make_metrics_entry();
        print_start(&entry, Verbosity::Normal, Some((1, 3)));
    }

    #[test]
    fn print_stop_with_position_does_not_panic() {
        let stats = ScenarioStats::default();
        print_stop(
            "test",
            Duration::from_secs(5),
            &stats,
            Verbosity::Normal,
            Some((2, 3)),
        );
    }

    // -----------------------------------------------------------------------
    // print_version: styled header does not panic
    // -----------------------------------------------------------------------

    #[test]
    fn print_version_styled_does_not_panic() {
        print_version();
    }
}
