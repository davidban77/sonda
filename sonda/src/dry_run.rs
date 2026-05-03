//! `--dry-run` rendering for v2 scenario files: pretty text and stable JSON.

use std::collections::HashMap;
use std::io::{self, Write};

use sonda_core::compiler::compile_after::{CompiledEntry, CompiledFile};
use sonda_core::compiler::timing::{
    self, constant_crossing_secs, csv_replay_crossing_secs, sawtooth_crossing_secs,
    sequence_crossing_secs, sine_crossing_secs, spike_crossing_secs, step_crossing_secs,
    uniform_crossing_secs, Operator, TimingError,
};
use sonda_core::compiler::{DelayClause, WhileClause, WhileOp};
use sonda_core::config::validate::parse_duration;
use sonda_core::generator::GeneratorConfig;

use crate::sink_format::sink_display;

/// Output format for the dry-run printer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DryRunFormat {
    /// Human-readable text rendering printed to stderr.
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

/// Print the dry-run output for a [`CompiledFile`].
///
/// Text output goes to stderr; JSON output goes to stdout.
pub fn print_dry_run_compiled(
    source_label: &str,
    compiled: &CompiledFile,
    format: DryRunFormat,
) -> anyhow::Result<()> {
    match format {
        DryRunFormat::Text => {
            let mut out = io::stderr().lock();
            write_text_compiled(&mut out, source_label, compiled)?;
        }
        DryRunFormat::Json => {
            let mut out = io::stdout().lock();
            write_json_compiled(&mut out, source_label, compiled)?;
        }
    }
    Ok(())
}

fn write_field<W: Write>(out: &mut W, label: &str, value: &str) -> io::Result<()> {
    writeln!(out, "    {label:<15} {value}")
}

fn write_phase_offset<W: Write>(out: &mut W, phase_offset: &Option<String>) -> io::Result<()> {
    if let Some(ref po) = phase_offset {
        write_field(out, "phase_offset:", po)?;
    }
    Ok(())
}

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

const INDETERMINATE_MARKER: &str = "<indeterminate — non-analytical generator>";

fn write_text_compiled<W: Write>(
    out: &mut W,
    source_label: &str,
    compiled: &CompiledFile,
) -> io::Result<()> {
    let entries = &compiled.entries;
    let total = entries.len();
    let scenario_word = if total == 1 { "scenario" } else { "scenarios" };
    writeln!(
        out,
        "[config] file: {source_label} (version: 2, {total} {scenario_word})"
    )?;

    let upstream_index = build_upstream_index(entries);

    for (i, entry) in entries.iter().enumerate() {
        writeln!(out)?;
        write_compiled_entry_text(out, entry, &upstream_index, i + 1, total)?;
        if i + 1 < total {
            writeln!(out, "---")?;
        }
    }

    writeln!(out)?;
    writeln!(out, "Validation: OK ({total} {scenario_word})")?;
    Ok(())
}

fn write_compiled_entry_text<W: Write>(
    out: &mut W,
    entry: &CompiledEntry,
    upstream: &HashMap<&str, &CompiledEntry>,
    index: usize,
    total: usize,
) -> io::Result<()> {
    writeln!(out, "[config] [{index}/{total}] {}", entry.name)?;
    writeln!(out)?;
    write_field(out, "name:", &entry.name)?;
    write_field(out, "signal:", &entry.signal_type)?;
    write_field(out, "rate:", &format!("{}/s", format_rate(entry.rate)))?;
    write_field(
        out,
        "duration:",
        entry.duration.as_deref().unwrap_or("indefinite"),
    )?;

    match entry.signal_type.as_str() {
        "metrics" => {
            if let Some(ref g) = entry.generator {
                write_field(out, "generator:", &generator_display(g))?;
            }
        }
        "logs" => {
            if let Some(ref g) = entry.log_generator {
                write_field(out, "generator:", &log_generator_display(g))?;
            }
        }
        "histogram" | "summary" => {
            if let Some(ref d) = entry.distribution {
                write_field(out, "distribution:", &format!("{d:?}"))?;
            }
        }
        _ => {}
    }

    write_field(out, "encoder:", &encoder_display(&entry.encoder))?;
    write_field(out, "sink:", &sink_display(&entry.sink))?;
    write_labels_btree(out, entry.labels.as_ref())?;
    write_phase_offset(out, &entry.phase_offset)?;
    write_clock_group(out, &entry.clock_group, Some(entry.clock_group_is_auto))?;
    write_while_block(out, entry, upstream)?;
    write_delay_block(out, entry.delay_clause.as_ref())?;
    Ok(())
}

fn write_labels_btree<W: Write>(
    out: &mut W,
    labels: Option<&std::collections::BTreeMap<String, String>>,
) -> io::Result<()> {
    if let Some(map) = labels {
        if !map.is_empty() {
            let rendered: Vec<String> = map.iter().map(|(k, v)| format!("{k}={v}")).collect();
            write_field(out, "labels:", &rendered.join(", "))?;
        }
    }
    Ok(())
}

fn write_while_block<W: Write>(
    out: &mut W,
    entry: &CompiledEntry,
    upstream: &HashMap<&str, &CompiledEntry>,
) -> io::Result<()> {
    let Some(ref clause) = entry.while_clause else {
        return Ok(());
    };
    write_field(out, "while:", &while_clause_display(clause))?;
    let upstream_entry = upstream.get(clause.ref_id.as_str()).copied();
    write_field(
        out,
        "first_open:",
        &first_open_display(upstream_entry, clause),
    )?;
    Ok(())
}

fn write_delay_block<W: Write>(out: &mut W, delay: Option<&DelayClause>) -> io::Result<()> {
    if let Some(delay) = delay {
        write_field(out, "delay:", &delay_clause_display(delay))?;
    }
    Ok(())
}

fn build_upstream_index(entries: &[CompiledEntry]) -> HashMap<&str, &CompiledEntry> {
    let mut map: HashMap<&str, &CompiledEntry> = HashMap::with_capacity(entries.len());
    for entry in entries {
        if let Some(ref id) = entry.id {
            map.entry(id.as_str()).or_insert(entry);
        }
    }
    map
}

fn while_clause_display(clause: &WhileClause) -> String {
    format!(
        "upstream='{}' op='{}' value={}",
        clause.ref_id,
        while_op_display(&clause.op),
        format_value(clause.value),
    )
}

fn delay_clause_display(delay: &DelayClause) -> String {
    let open = delay
        .open
        .map(|d| format!("{}s", d.as_secs_f64()))
        .unwrap_or_else(|| "0s".to_string());
    let close = delay
        .close
        .map(|d| format!("{}s", d.as_secs_f64()))
        .unwrap_or_else(|| "0s".to_string());
    format!("open={open} close={close}")
}

fn first_open_display(upstream: Option<&CompiledEntry>, clause: &WhileClause) -> String {
    let Some(upstream) = upstream else {
        return INDETERMINATE_MARKER.to_string();
    };
    let Some(ref generator) = upstream.generator else {
        return INDETERMINATE_MARKER.to_string();
    };
    let op = match clause.op {
        WhileOp::LessThan => Operator::LessThan,
        WhileOp::GreaterThan => Operator::GreaterThan,
    };
    match crossing_secs(generator, op, clause.value, upstream.rate) {
        Ok(secs) => format!("~{}s", format_secs(secs)),
        Err(_) => INDETERMINATE_MARKER.to_string(),
    }
}

fn while_op_display(op: &WhileOp) -> &'static str {
    match op {
        WhileOp::LessThan => "<",
        WhileOp::GreaterThan => ">",
    }
}

fn format_value(v: f64) -> String {
    if v.fract().abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn format_secs(secs: f64) -> String {
    if secs.fract().abs() < f64::EPSILON {
        format!("{}", secs as i64)
    } else {
        format!("{secs:.2}")
    }
}

fn crossing_secs(
    generator: &GeneratorConfig,
    op: Operator,
    threshold: f64,
    rate: f64,
) -> Result<f64, TimingError> {
    match generator {
        GeneratorConfig::Constant { value } => constant_crossing_secs(op, threshold, *value),
        GeneratorConfig::Uniform { .. } => uniform_crossing_secs(),
        GeneratorConfig::Sine { .. } => sine_crossing_secs(),
        GeneratorConfig::CsvReplay { .. } => csv_replay_crossing_secs(),
        GeneratorConfig::Sawtooth {
            min,
            max,
            period_secs,
        } => sawtooth_crossing_secs(op, threshold, *min, *max, *period_secs),
        GeneratorConfig::Sequence { values, repeat } => {
            sequence_crossing_secs(op, threshold, values, *repeat, rate)
        }
        GeneratorConfig::Step {
            start,
            step_size,
            max,
        } => step_crossing_secs(op, threshold, start.unwrap_or(0.0), *step_size, *max, rate),
        GeneratorConfig::Spike {
            baseline,
            magnitude,
            duration_secs,
            ..
        } => spike_crossing_secs(op, threshold, *baseline, *magnitude, *duration_secs),
        GeneratorConfig::Flap {
            up_duration,
            down_duration,
            up_value,
            down_value,
        } => {
            let up_secs = duration_or_default(up_duration.as_deref(), 10.0)?;
            let down_secs = duration_or_default(down_duration.as_deref(), 5.0)?;
            timing::flap_crossing_secs(
                op,
                threshold,
                up_secs,
                down_secs,
                up_value.unwrap_or(1.0),
                down_value.unwrap_or(0.0),
            )
        }
        GeneratorConfig::Saturation {
            baseline,
            ceiling,
            time_to_saturate,
        } => sawtooth_crossing_secs(
            op,
            threshold,
            baseline.unwrap_or(0.0),
            ceiling.unwrap_or(100.0),
            duration_or_default(time_to_saturate.as_deref(), 5.0 * 60.0)?,
        ),
        GeneratorConfig::Leak {
            baseline,
            ceiling,
            time_to_ceiling,
        } => sawtooth_crossing_secs(
            op,
            threshold,
            baseline.unwrap_or(0.0),
            ceiling.unwrap_or(100.0),
            duration_or_default(time_to_ceiling.as_deref(), 10.0 * 60.0)?,
        ),
        GeneratorConfig::Degradation {
            baseline,
            ceiling,
            time_to_degrade,
            ..
        } => sawtooth_crossing_secs(
            op,
            threshold,
            baseline.unwrap_or(0.0),
            ceiling.unwrap_or(100.0),
            duration_or_default(time_to_degrade.as_deref(), 5.0 * 60.0)?,
        ),
        GeneratorConfig::Steady { .. } => timing::steady_crossing_secs(),
        GeneratorConfig::SpikeEvent {
            baseline,
            spike_height,
            spike_duration,
            ..
        } => spike_crossing_secs(
            op,
            threshold,
            baseline.unwrap_or(0.0),
            spike_height.unwrap_or(100.0),
            duration_or_default(spike_duration.as_deref(), 10.0)?,
        ),
        _ => Err(TimingError::Unsupported {
            message: "unknown generator".to_string(),
        }),
    }
}

fn duration_or_default(input: Option<&str>, default_secs: f64) -> Result<f64, TimingError> {
    match input {
        Some(s) => {
            parse_duration(s)
                .map(|d| d.as_secs_f64())
                .map_err(|e| TimingError::InvalidDuration {
                    field: "duration",
                    input: s.to_string(),
                    reason: e.to_string(),
                })
        }
        None => Ok(default_secs),
    }
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    while_clause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delay_clause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_open: Option<String>,
}

fn write_json_compiled<W: Write>(
    out: &mut W,
    source_label: &str,
    compiled: &CompiledFile,
) -> io::Result<()> {
    let upstream = build_upstream_index(&compiled.entries);
    let scenarios = compiled
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| to_compiled_scenario_dto(i + 1, entry, &upstream))
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

fn to_compiled_scenario_dto<'a>(
    index: usize,
    entry: &'a CompiledEntry,
    upstream: &HashMap<&str, &CompiledEntry>,
) -> ScenarioDto<'a> {
    let signal: &'static str = match entry.signal_type.as_str() {
        "metrics" => "metrics",
        "logs" => "logs",
        "histogram" => "histogram",
        "summary" => "summary",
        _ => "unknown",
    };
    let generator = if let Some(ref g) = entry.generator {
        generator_display(g)
    } else if let Some(ref g) = entry.log_generator {
        log_generator_display(g)
    } else if let Some(ref d) = entry.distribution {
        format!("{d:?}")
    } else {
        "unknown".to_string()
    };
    let labels = entry
        .labels
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let while_clause = entry.while_clause.as_ref().map(while_clause_display);
    let delay_clause = entry.delay_clause.as_ref().map(delay_clause_display);
    let first_open = entry.while_clause.as_ref().map(|clause| {
        let upstream_entry = upstream.get(clause.ref_id.as_str()).copied();
        first_open_display(upstream_entry, clause)
    });

    ScenarioDto {
        index,
        name: entry.name.as_str(),
        signal,
        rate: entry.rate,
        duration: entry.duration.as_deref(),
        generator,
        encoder: encoder_display(&entry.encoder),
        sink: sink_display(&entry.sink),
        labels,
        phase_offset: entry.phase_offset.as_deref(),
        clock_group: entry.clock_group.as_deref(),
        clock_group_is_auto: Some(entry.clock_group_is_auto),
        while_clause,
        delay_clause,
        first_open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compile_scenario_file_compiled;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    fn compile(yaml: &str) -> CompiledFile {
        compile_scenario_file_compiled(yaml, &InMemoryPackResolver::new()).expect("must compile")
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
        let compiled = compile(
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
        write_text_compiled(&mut buf, "scn.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("[config] file: scn.yaml (version: 2, 1 scenario)"));
        assert!(out.contains("Validation: OK (1 scenario)"));
    }

    #[test]
    fn text_pluralizes_count_when_multi_scenario() {
        let compiled = compile(
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
        write_text_compiled(&mut buf, "multi.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("(version: 2, 2 scenarios)"));
        assert!(out.contains("Validation: OK (2 scenarios)"));
        assert!(out.contains("\n---\n"));
    }

    #[test]
    fn text_prints_phase_offset_and_clock_group_for_after_chain() {
        let compiled = compile(
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
        write_text_compiled(&mut buf, "link-failover.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("phase_offset:") && out.contains("60"),
            "phase_offset line must render, got:\n{out}"
        );
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
        let compiled = compile(
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
        write_json_compiled(&mut buf, "scn.yaml", &compiled).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).expect("json parses");
        assert_eq!(json["file"], "scn.yaml");
        assert_eq!(json["version"], 2);
        assert_eq!(json["scenarios"][0]["name"], "cpu_usage");
        assert_eq!(json["scenarios"][0]["signal"], "metrics");
        assert_eq!(json["scenarios"][0]["rate"], 2.0);
        assert_eq!(json["scenarios"][0]["labels"]["host"], "t0");
    }

    #[test]
    fn text_renders_while_block_with_first_open_for_analytical_upstream() {
        let compiled = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0

  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#,
        );
        let mut buf = Vec::new();
        write_text_compiled(&mut buf, "while-analytical.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("while:"),
            "must render while block, got:\n{out}"
        );
        assert!(
            out.contains("upstream='link' op='>' value=50"),
            "must render while clause body, got:\n{out}"
        );
        assert!(
            out.contains("first_open:") && out.contains("~30s"),
            "must render analytical first_open, got:\n{out}"
        );
    }

    #[test]
    fn text_renders_indeterminate_marker_for_non_analytical_upstream() {
        let compiled = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 1m
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60.0
      offset: 50.0

  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#,
        );
        let mut buf = Vec::new();
        write_text_compiled(&mut buf, "while-non-analytical.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("<indeterminate — non-analytical generator>"),
            "non-analytical upstream must render the indeterminate marker, got:\n{out}"
        );
    }

    #[test]
    fn text_renders_delay_block_when_present() {
        let compiled = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0

  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
    delay:
      open: "5s"
      close: "10s"
"#,
        );
        let mut buf = Vec::new();
        write_text_compiled(&mut buf, "while-delay.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("delay:") && out.contains("open=5s") && out.contains("close=10s"),
            "delay block must render, got:\n{out}"
        );
    }

    #[test]
    fn text_renders_both_after_and_while_for_mixed_upstream() {
        let compiled = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: trigger
    signal_type: metrics
    name: trigger_metric
    generator:
      type: step
      start: 0.0
      step_size: 1.0

  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0

  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    after:
      ref: trigger
      op: ">"
      value: 5.0
    while:
      ref: link
      op: ">"
      value: 50.0
"#,
        );
        let mut buf = Vec::new();
        write_text_compiled(&mut buf, "mixed-upstream.yaml", &compiled).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("phase_offset:"),
            "after-derived phase_offset must render, got:\n{out}"
        );
        assert!(
            out.contains("while:") && out.contains("upstream='link'"),
            "while block must render, got:\n{out}"
        );
        assert!(
            out.contains("first_open:"),
            "first_open must render alongside, got:\n{out}"
        );
    }

    #[test]
    fn json_dto_includes_while_delay_first_open_for_gated_entry() {
        let compiled = compile(
            r#"version: 2
defaults:
  rate: 1
  duration: 5m
scenarios:
  - id: link
    signal_type: metrics
    name: link_state
    generator:
      type: sawtooth
      min: 0.0
      max: 100.0
      period_secs: 60.0

  - id: traffic
    signal_type: metrics
    name: backup_traffic
    generator:
      type: constant
      value: 50.0
    while:
      ref: link
      op: ">"
      value: 50.0
    delay:
      open: "5s"
      close: "10s"
"#,
        );
        let mut buf = Vec::new();
        write_json_compiled(&mut buf, "while-json.yaml", &compiled).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        let traffic = json["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"].as_str() == Some("backup_traffic"))
            .expect("traffic entry must exist");
        assert_eq!(
            traffic["while_clause"].as_str().unwrap(),
            "upstream='link' op='>' value=50"
        );
        assert_eq!(
            traffic["delay_clause"].as_str().unwrap(),
            "open=5s close=10s"
        );
        assert_eq!(traffic["first_open"].as_str().unwrap(), "~30s");
    }

    #[test]
    fn json_dto_omits_clauses_when_absent() {
        let compiled = compile(
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
        write_json_compiled(&mut buf, "no-clauses.yaml", &compiled).unwrap();
        let body = String::from_utf8(buf).unwrap();
        assert!(
            !body.contains("while_clause"),
            "while_clause must be omitted when None, got:\n{body}"
        );
        assert!(
            !body.contains("delay_clause"),
            "delay_clause must be omitted when None, got:\n{body}"
        );
        assert!(
            !body.contains("first_open"),
            "first_open must be omitted when None, got:\n{body}"
        );
    }
}
