//! Spec §5 `--dry-run` output for v2 scenario files.
//!
//! The v2 compiler resolves defaults, expands packs, computes `after:`
//! crossing times, and assigns clock groups. Users need a readable view of
//! that resolved representation to debug their scenarios. This module
//! formats [`Vec<ScenarioEntry>`][sonda_core::ScenarioEntry] — the output
//! of [`sonda_core::compile_scenario_file`] — in the pretty format the
//! spec prescribes, plus a stable JSON DTO for machine-readable consumption.
//!
//! v1 `--dry-run` output is unchanged. This module is invoked only when
//! `scenario_loader::load_scenario_entries` reports `Some(2)` for the
//! version.

use std::io::{self, Write};

use sonda_core::config::{
    HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};

use crate::sink_format::sink_display;

/// Output format for the dry-run printer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DryRunFormat {
    /// Human-readable spec §5 format printed to stderr.
    #[default]
    Text,
    /// Stable JSON DTO printed to stdout.
    Json,
}

/// Parse a user-supplied `--format` value into a [`DryRunFormat`].
///
/// Accepts `"text"` (default) or `"json"`. Any other value surfaces as an
/// error. `None` resolves to [`DryRunFormat::Text`].
pub fn parse_format(value: Option<&str>) -> anyhow::Result<DryRunFormat> {
    match value {
        None | Some("text") => Ok(DryRunFormat::Text),
        Some("json") => Ok(DryRunFormat::Json),
        Some(other) => Err(anyhow::anyhow!(
            "invalid --format {other:?}; valid values: text, json"
        )),
    }
}

/// Print the spec §5 dry-run output for a list of compiled scenario entries.
///
/// - `source_label` is a user-facing identifier for the scenario file
///   (typically the path string or `@name`); it is shown verbatim in the
///   header line.
/// - `entries` is the fully-compiled runtime input produced by
///   [`sonda_core::compile_scenario_file`].
/// - `format` selects between the spec §5 pretty output and the JSON DTO.
///
/// Text output goes to stderr; JSON output goes to stdout. This matches the
/// CLI convention of "data on stdout, diagnostics on stderr".
pub fn print_dry_run(
    source_label: &str,
    entries: &[ScenarioEntry],
    format: DryRunFormat,
) -> anyhow::Result<()> {
    match format {
        DryRunFormat::Text => {
            let mut out = io::stderr().lock();
            write_text(&mut out, source_label, entries)?;
        }
        DryRunFormat::Json => {
            let mut out = io::stdout().lock();
            write_json(&mut out, source_label, entries)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------------

/// Write the spec §5 pretty output.
///
/// Separated from [`print_dry_run`] so tests can capture the output
/// into a `Vec<u8>` for snapshot assertions without mocking stderr.
pub fn write_text<W: Write>(
    out: &mut W,
    source_label: &str,
    entries: &[ScenarioEntry],
) -> io::Result<()> {
    let total = entries.len();
    let scenario_word = if total == 1 { "scenario" } else { "scenarios" };
    writeln!(
        out,
        "[config] file: {source_label} (version: 2, {total} {scenario_word})"
    )?;

    for (i, entry) in entries.iter().enumerate() {
        writeln!(out)?;
        write_entry_text(out, entry, i + 1, total)?;
        if i + 1 < total {
            writeln!(out, "---")?;
        }
    }

    writeln!(out)?;
    writeln!(out, "Validation: OK ({total} {scenario_word})")?;
    Ok(())
}

/// Write a single compiled entry in the spec §5 "one block per scenario"
/// format.
fn write_entry_text<W: Write>(
    out: &mut W,
    entry: &ScenarioEntry,
    index: usize,
    total: usize,
) -> io::Result<()> {
    let name = entry.base().name.as_str();
    writeln!(out, "[config] [{index}/{total}] {name}")?;
    writeln!(out)?;
    match entry {
        ScenarioEntry::Metrics(c) => write_metrics_fields(out, c)?,
        ScenarioEntry::Logs(c) => write_logs_fields(out, c)?,
        ScenarioEntry::Histogram(c) => write_histogram_fields(out, c)?,
        ScenarioEntry::Summary(c) => write_summary_fields(out, c)?,
        // `ScenarioEntry` is `#[non_exhaustive]` across the crate boundary;
        // emit a marker line so future variants render rather than panic.
        _ => write_field(out, "signal:", "unknown")?,
    }
    Ok(())
}

fn write_metrics_fields<W: Write>(out: &mut W, c: &ScenarioConfig) -> io::Result<()> {
    write_field(out, "name:", c.name.as_str())?;
    write_field(out, "signal:", "metrics")?;
    write_field(out, "rate:", &format!("{}/s", format_rate(c.rate)))?;
    write_field(
        out,
        "duration:",
        c.duration.as_deref().unwrap_or("indefinite"),
    )?;
    write_field(out, "generator:", &generator_display(&c.generator))?;
    write_field(out, "encoder:", &encoder_display(&c.encoder))?;
    write_field(out, "sink:", &sink_display(&c.sink))?;
    write_labels(out, &c.labels)?;
    write_phase_offset(out, &c.phase_offset)?;
    write_clock_group(out, &c.clock_group, c.clock_group_is_auto)?;
    Ok(())
}

fn write_logs_fields<W: Write>(out: &mut W, c: &LogScenarioConfig) -> io::Result<()> {
    write_field(out, "name:", c.name.as_str())?;
    write_field(out, "signal:", "logs")?;
    write_field(out, "rate:", &format!("{}/s", format_rate(c.rate)))?;
    write_field(
        out,
        "duration:",
        c.duration.as_deref().unwrap_or("indefinite"),
    )?;
    write_field(out, "generator:", &log_generator_display(&c.generator))?;
    write_field(out, "encoder:", &encoder_display(&c.encoder))?;
    write_field(out, "sink:", &sink_display(&c.sink))?;
    write_labels(out, &c.labels)?;
    write_phase_offset(out, &c.phase_offset)?;
    write_clock_group(out, &c.clock_group, c.clock_group_is_auto)?;
    Ok(())
}

fn write_histogram_fields<W: Write>(out: &mut W, c: &HistogramScenarioConfig) -> io::Result<()> {
    write_field(out, "name:", c.name.as_str())?;
    write_field(out, "signal:", "histogram")?;
    write_field(out, "rate:", &format!("{}/s", format_rate(c.rate)))?;
    write_field(
        out,
        "duration:",
        c.duration.as_deref().unwrap_or("indefinite"),
    )?;
    write_field(out, "distribution:", &format!("{:?}", c.distribution))?;
    write_field(out, "encoder:", &encoder_display(&c.encoder))?;
    write_field(out, "sink:", &sink_display(&c.sink))?;
    write_labels(out, &c.labels)?;
    write_phase_offset(out, &c.phase_offset)?;
    write_clock_group(out, &c.clock_group, c.clock_group_is_auto)?;
    Ok(())
}

fn write_summary_fields<W: Write>(out: &mut W, c: &SummaryScenarioConfig) -> io::Result<()> {
    write_field(out, "name:", c.name.as_str())?;
    write_field(out, "signal:", "summary")?;
    write_field(out, "rate:", &format!("{}/s", format_rate(c.rate)))?;
    write_field(
        out,
        "duration:",
        c.duration.as_deref().unwrap_or("indefinite"),
    )?;
    write_field(out, "distribution:", &format!("{:?}", c.distribution))?;
    write_field(out, "encoder:", &encoder_display(&c.encoder))?;
    write_field(out, "sink:", &sink_display(&c.sink))?;
    write_labels(out, &c.labels)?;
    write_phase_offset(out, &c.phase_offset)?;
    write_clock_group(out, &c.clock_group, c.clock_group_is_auto)?;
    Ok(())
}

fn write_field<W: Write>(out: &mut W, label: &str, value: &str) -> io::Result<()> {
    writeln!(out, "    {label:<15} {value}")
}

fn write_labels<W: Write>(
    out: &mut W,
    labels: &Option<std::collections::HashMap<String, String>>,
) -> io::Result<()> {
    if let Some(ref map) = labels {
        if !map.is_empty() {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(a, _)| *a);
            let rendered: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            write_field(out, "labels:", &rendered.join(", "))?;
        }
    }
    Ok(())
}

fn write_phase_offset<W: Write>(out: &mut W, phase_offset: &Option<String>) -> io::Result<()> {
    if let Some(ref po) = phase_offset {
        write_field(out, "phase_offset:", po)?;
    }
    Ok(())
}

/// Render the `clock_group:` line.
///
/// When `is_auto` is `Some(true)` — meaning the v2 compiler synthesized
/// the value because the entry's `after:` component had no explicit
/// override — append a trailing ` (auto)` marker so users can tell the
/// auto-name apart from a value they wrote themselves. Explicit values
/// (including ones that happen to start with `chain_`) and entries that
/// never traversed the v2 compiler render bare.
fn write_clock_group<W: Write>(
    out: &mut W,
    clock_group: &Option<String>,
    is_auto: Option<bool>,
) -> io::Result<()> {
    if let Some(ref cg) = clock_group {
        let rendered = if is_auto == Some(true) {
            format!("{cg} (auto)")
        } else {
            cg.clone()
        };
        write_field(out, "clock_group:", &rendered)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Field formatters — lightweight shims over the runtime config types
// ---------------------------------------------------------------------------

fn format_rate(rate: f64) -> String {
    if (rate.fract()).abs() < f64::EPSILON {
        format!("{}", rate as i64)
    } else {
        format!("{rate}")
    }
}

fn generator_display(gen: &sonda_core::generator::GeneratorConfig) -> String {
    use sonda_core::generator::GeneratorConfig;
    match gen {
        GeneratorConfig::Constant { value } => format!("constant (value: {value})"),
        GeneratorConfig::Uniform { min, max, .. } => format!("uniform (min: {min}, max: {max})"),
        GeneratorConfig::Sine {
            amplitude,
            period_secs,
            offset,
        } => format!("sine (amplitude: {amplitude}, period_secs: {period_secs}, offset: {offset})"),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => {
            format!("sawtooth (min: {min}, max: {max}, period_secs: {period_secs})")
        }
        GeneratorConfig::Sequence { values, .. } => format!("sequence ({} values)", values.len()),
        GeneratorConfig::Step {
            start, step_size, ..
        } => {
            let start_val = start.unwrap_or(0.0);
            format!("step (start: {start_val}, step_size: {step_size})")
        }
        GeneratorConfig::Spike {
            baseline,
            magnitude,
            ..
        } => {
            format!("spike (baseline: {baseline}, magnitude: {magnitude})")
        }
        GeneratorConfig::CsvReplay { file, .. } => format!("csv_replay (file: {file})"),
        GeneratorConfig::Flap {
            up_duration,
            down_duration,
            up_value,
            down_value,
        } => {
            let up_d = up_duration.as_deref().unwrap_or("10s");
            let dn_d = down_duration.as_deref().unwrap_or("5s");
            let up_v = up_value.unwrap_or(1.0);
            let dn_v = down_value.unwrap_or(0.0);
            format!(
                "flap (up_duration: {up_d}, down_duration: {dn_d}, up_value: {up_v}, down_value: {dn_v})"
            )
        }
        GeneratorConfig::Saturation {
            baseline,
            ceiling,
            time_to_saturate,
        } => {
            let baseline_v = baseline.unwrap_or(0.0);
            let ceiling_v = ceiling.unwrap_or(100.0);
            let tts = time_to_saturate.as_deref().unwrap_or("5m");
            format!(
                "saturation (baseline: {baseline_v}, ceiling: {ceiling_v}, time_to_saturate: {tts})"
            )
        }
        GeneratorConfig::Leak {
            baseline,
            ceiling,
            time_to_ceiling,
        } => {
            let baseline_v = baseline.unwrap_or(0.0);
            let ceiling_v = ceiling.unwrap_or(100.0);
            let ttc = time_to_ceiling.as_deref().unwrap_or("10m");
            format!("leak (baseline: {baseline_v}, ceiling: {ceiling_v}, time_to_ceiling: {ttc})")
        }
        GeneratorConfig::Degradation {
            baseline,
            ceiling,
            time_to_degrade,
            ..
        } => {
            let baseline_v = baseline.unwrap_or(0.0);
            let ceiling_v = ceiling.unwrap_or(100.0);
            let ttd = time_to_degrade.as_deref().unwrap_or("5m");
            format!(
                "degradation (baseline: {baseline_v}, ceiling: {ceiling_v}, time_to_degrade: {ttd})"
            )
        }
        GeneratorConfig::SpikeEvent {
            baseline,
            spike_height,
            ..
        } => {
            let baseline_v = baseline.unwrap_or(0.0);
            let height_v = spike_height.unwrap_or(100.0);
            format!("spike_event (baseline: {baseline_v}, spike_height: {height_v})")
        }
        GeneratorConfig::Steady {
            center, amplitude, ..
        } => {
            let center_v = center.unwrap_or(50.0);
            let amp_v = amplitude.unwrap_or(10.0);
            format!("steady (center: {center_v}, amplitude: {amp_v})")
        }
        // `GeneratorConfig` is `#[non_exhaustive]` across the crate boundary;
        // fall back to the Debug form so a future variant still renders.
        other => format!("unknown ({other:?})"),
    }
}

fn log_generator_display(gen: &sonda_core::generator::LogGeneratorConfig) -> String {
    use sonda_core::generator::LogGeneratorConfig;
    match gen {
        LogGeneratorConfig::Template { templates, .. } => {
            format!("template ({} templates)", templates.len())
        }
        LogGeneratorConfig::Replay { file } => format!("replay (file: {file})"),
    }
}

fn encoder_display(enc: &sonda_core::encoder::EncoderConfig) -> String {
    use sonda_core::encoder::EncoderConfig;
    match enc {
        EncoderConfig::PrometheusText { .. } => "prometheus_text".to_string(),
        EncoderConfig::InfluxLineProtocol { .. } => "influx_lp".to_string(),
        EncoderConfig::JsonLines { .. } => "json_lines".to_string(),
        EncoderConfig::Syslog { .. } => "syslog".to_string(),
        #[cfg(feature = "remote-write")]
        EncoderConfig::RemoteWrite => "remote_write".to_string(),
        #[cfg(not(feature = "remote-write"))]
        EncoderConfig::RemoteWriteDisabled {} => "remote_write (disabled)".to_string(),
        #[cfg(feature = "otlp")]
        EncoderConfig::Otlp => "otlp".to_string(),
        #[cfg(not(feature = "otlp"))]
        EncoderConfig::OtlpDisabled {} => "otlp (disabled)".to_string(),
        // `EncoderConfig` is `#[non_exhaustive]` across the crate boundary;
        // fall back to the Debug form so a future variant still renders.
        other => format!("unknown ({other:?})"),
    }
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

/// Stable JSON DTO shape for machine-readable `--dry-run --format=json`.
///
/// Fields are intentionally stringly-typed (rather than raw enum
/// variants) so downstream consumers can parse the output without
/// depending on sonda-core. Keys are sorted at emit time for
/// determinism.
#[derive(Debug, serde::Serialize)]
struct DryRunDto<'a> {
    file: &'a str,
    version: u32,
    scenarios: Vec<ScenarioDto<'a>>,
}

#[derive(Debug, serde::Serialize)]
struct ScenarioDto<'a> {
    index: usize,
    name: &'a str,
    signal: &'static str,
    rate: f64,
    duration: Option<&'a str>,
    generator: String,
    encoder: String,
    sink: String,
    labels: std::collections::BTreeMap<String, String>,
    phase_offset: Option<&'a str>,
    clock_group: Option<&'a str>,
    /// Compiler-derived provenance for [`Self::clock_group`]. Mirrors
    /// [`sonda_core::config::BaseScheduleConfig::clock_group_is_auto`]:
    /// `Some(true)` when the v2 compiler synthesized the value,
    /// `Some(false)` when it was an explicit user assignment, `None`
    /// when the entry did not flow through the v2 compiler. Suppressed
    /// from JSON when `None` so v1-loaded entries don't see a noise
    /// field.
    #[serde(skip_serializing_if = "Option::is_none")]
    clock_group_is_auto: Option<bool>,
}

fn write_json<W: Write>(
    out: &mut W,
    source_label: &str,
    entries: &[ScenarioEntry],
) -> io::Result<()> {
    let scenarios = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| to_scenario_dto(i + 1, entry))
        .collect();

    let dto = DryRunDto {
        file: source_label,
        version: 2,
        scenarios,
    };

    let serialized = serde_json::to_string_pretty(&dto).map_err(io::Error::other)?;
    out.write_all(serialized.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(())
}

fn to_scenario_dto(index: usize, entry: &ScenarioEntry) -> ScenarioDto<'_> {
    match entry {
        ScenarioEntry::Metrics(c) => ScenarioDto {
            index,
            name: c.name.as_str(),
            signal: "metrics",
            rate: c.rate,
            duration: c.duration.as_deref(),
            generator: generator_display(&c.generator),
            encoder: encoder_display(&c.encoder),
            sink: sink_display(&c.sink),
            labels: labels_btree(&c.labels),
            phase_offset: c.phase_offset.as_deref(),
            clock_group: c.clock_group.as_deref(),
            clock_group_is_auto: c.clock_group_is_auto,
        },
        ScenarioEntry::Logs(c) => ScenarioDto {
            index,
            name: c.name.as_str(),
            signal: "logs",
            rate: c.rate,
            duration: c.duration.as_deref(),
            generator: log_generator_display(&c.generator),
            encoder: encoder_display(&c.encoder),
            sink: sink_display(&c.sink),
            labels: labels_btree(&c.labels),
            phase_offset: c.phase_offset.as_deref(),
            clock_group: c.clock_group.as_deref(),
            clock_group_is_auto: c.clock_group_is_auto,
        },
        ScenarioEntry::Histogram(c) => ScenarioDto {
            index,
            name: c.name.as_str(),
            signal: "histogram",
            rate: c.rate,
            duration: c.duration.as_deref(),
            generator: format!("{:?}", c.distribution),
            encoder: encoder_display(&c.encoder),
            sink: sink_display(&c.sink),
            labels: labels_btree(&c.labels),
            phase_offset: c.phase_offset.as_deref(),
            clock_group: c.clock_group.as_deref(),
            clock_group_is_auto: c.clock_group_is_auto,
        },
        ScenarioEntry::Summary(c) => ScenarioDto {
            index,
            name: c.name.as_str(),
            signal: "summary",
            rate: c.rate,
            duration: c.duration.as_deref(),
            generator: format!("{:?}", c.distribution),
            encoder: encoder_display(&c.encoder),
            sink: sink_display(&c.sink),
            labels: labels_btree(&c.labels),
            phase_offset: c.phase_offset.as_deref(),
            clock_group: c.clock_group.as_deref(),
            clock_group_is_auto: c.clock_group_is_auto,
        },
        // `ScenarioEntry` is `#[non_exhaustive]` across the crate boundary;
        // borrow schedule-level fields via `base()` so a future variant still
        // round-trips through the JSON DTO with a marker signal label.
        other => {
            let base = other.base();
            ScenarioDto {
                index,
                name: base.name.as_str(),
                signal: "unknown",
                rate: base.rate,
                duration: base.duration.as_deref(),
                generator: format!("unknown ({other:?})"),
                encoder: String::from("unknown"),
                sink: sink_display(&base.sink),
                labels: labels_btree(&base.labels),
                phase_offset: base.phase_offset.as_deref(),
                clock_group: base.clock_group.as_deref(),
                clock_group_is_auto: base.clock_group_is_auto,
            }
        }
    }
}

fn labels_btree(
    labels: &Option<std::collections::HashMap<String, String>>,
) -> std::collections::BTreeMap<String, String> {
    match labels {
        Some(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        None => std::collections::BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compile_scenario_file;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    fn compile(yaml: &str) -> Vec<ScenarioEntry> {
        compile_scenario_file(yaml, &InMemoryPackResolver::new()).expect("must compile")
    }

    #[test]
    fn parse_format_defaults_to_text() {
        assert_eq!(parse_format(None).unwrap(), DryRunFormat::Text);
    }

    #[test]
    fn parse_format_accepts_text() {
        assert_eq!(parse_format(Some("text")).unwrap(), DryRunFormat::Text);
    }

    #[test]
    fn parse_format_accepts_json() {
        assert_eq!(parse_format(Some("json")).unwrap(), DryRunFormat::Json);
    }

    #[test]
    fn parse_format_rejects_unknown_value() {
        let err = parse_format(Some("xml")).expect_err("unknown format must fail");
        let msg = format!("{err}");
        assert!(msg.contains("xml"), "error mentions the bad value: {msg}");
    }

    #[test]
    fn text_header_includes_file_version_and_count() {
        let entries = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: a
    signal_type: metrics
    name: metric_a
    generator:
      type: constant
      value: 1.0
"#,
        );
        let mut buf = Vec::new();
        write_text(&mut buf, "scn.yaml", &entries).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("[config] file: scn.yaml (version: 2, 1 scenario)"));
        assert!(out.contains("Validation: OK (1 scenario)"));
    }

    #[test]
    fn text_pluralizes_count_when_multi_scenario() {
        let entries = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 100ms
scenarios:
  - id: a
    signal_type: metrics
    name: metric_a
    generator:
      type: constant
      value: 1.0
  - id: b
    signal_type: metrics
    name: metric_b
    generator:
      type: constant
      value: 2.0
"#,
        );
        let mut buf = Vec::new();
        write_text(&mut buf, "multi.yaml", &entries).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("(version: 2, 2 scenarios)"));
        assert!(out.contains("Validation: OK (2 scenarios)"));
        // Separator between blocks.
        assert!(out.contains("\n---\n"));
    }

    #[test]
    fn text_prints_phase_offset_and_clock_group_for_after_chain() {
        let entries = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: primary_link
    signal_type: metrics
    name: interface_oper_state
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s

  - id: backup_util
    signal_type: metrics
    name: backup_link_utilization
    generator:
      type: saturation
      baseline: 20
      ceiling: 85
      time_to_saturate: 2m
    after:
      ref: primary_link
      op: "<"
      value: 1
"#,
        );
        let mut buf = Vec::new();
        write_text(&mut buf, "link-failover.yaml", &entries).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // backup_util's after-derived offset is 60s (flap up_duration).
        assert!(
            out.contains("phase_offset:") && out.contains("60"),
            "phase_offset line must render, got:\n{out}"
        );
        // Auto clock_group is `chain_{lowest_lex_id}` across the connected
        // component; `backup_util` < `primary_link` alphabetically.
        assert!(
            out.contains("chain_backup_util"),
            "auto clock_group must render, got:\n{out}"
        );
        assert!(
            out.contains("(auto)"),
            "auto marker must render, got:\n{out}"
        );
    }

    #[test]
    fn json_output_has_stable_shape() {
        let entries = compile(
            r#"version: 2
defaults:
  rate: 2
  duration: 500ms
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
    labels:
      host: t0
"#,
        );
        let mut buf = Vec::new();
        write_json(&mut buf, "scn.yaml", &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).expect("json parses");
        assert_eq!(json["file"], "scn.yaml");
        assert_eq!(json["version"], 2);
        assert_eq!(json["scenarios"][0]["name"], "cpu_usage");
        assert_eq!(json["scenarios"][0]["signal"], "metrics");
        assert_eq!(json["scenarios"][0]["rate"], 2.0);
        assert_eq!(json["scenarios"][0]["labels"]["host"], "t0");
    }
}
