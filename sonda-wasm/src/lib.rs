//! WebAssembly facade over the sonda-core engine.
//!
//! Powers the documentation-site playground: scenario YAML goes in, and the
//! real compiler + generators produce per-entry sampled values, schedule
//! windows, and an encoded output preview. Because this is the same code path
//! `sonda run` uses, the playground doubles as a validator — compile errors
//! come back with their real messages.
//!
//! Only the pure engine is linked (`sonda-core` without the `runtime`
//! feature), so the crate compiles to `wasm32-unknown-unknown`. Log entries
//! sample through the clock-free `LogGenerator::generate_at` path with
//! synthesized timestamps; entries that need the filesystem in the browser
//! (metric and log `csv_replay`) are reported as skipped rather than
//! sampled.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use wasm_bindgen::prelude::wasm_bindgen;

use sonda_core::compiler::compile_after::CompiledEntry;
use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::validate::{
    parse_duration, validate_histogram_config, validate_summary_config,
};
use sonda_core::config::{
    BaseScheduleConfig, HistogramScenarioConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry,
    SummaryScenarioConfig,
};
use sonda_core::encoder::create_encoder;
use sonda_core::generator::histogram::HistogramGenerator;
use sonda_core::generator::summary::SummaryGenerator;
use sonda_core::generator::{
    create_generator, create_log_generator, JitterWrapper, LogGeneratorConfig, ValueGenerator,
};
use sonda_core::model::log::Severity;
use sonda_core::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use sonda_core::{compile_scenario_file, compile_scenario_file_compiled, desugar_entry};

/// Fixed base timestamp for encoded previews (milliseconds since epoch).
///
/// The browser sandbox has no wall clock (`SystemTime::now()` panics on
/// `wasm32-unknown-unknown`), and a stable timestamp keeps preview output
/// deterministic for identical scenarios.
const PREVIEW_EPOCH_MS: u64 = 1_777_000_000_000;

/// How many encoded lines the preview pane shows per entry.
const PREVIEW_EVENTS: usize = 5;

/// How many rendered log lines the logs pane shows per entry.
const LOG_LINES_CAP: u32 = 240;

/// Result envelope returned to JavaScript as JSON.
#[derive(Serialize)]
struct SampleResult {
    ok: bool,
    /// Compile error with the engine's real message, when `ok` is false.
    error: Option<String>,
    entries: Vec<EntrySample>,
    histograms: Vec<HistogramEntrySample>,
    summaries: Vec<SummaryEntrySample>,
    logs: Vec<LogEntrySample>,
    skipped: Vec<SkippedEntry>,
}

/// One sampled log entry — rendered lines for the logs pane plus an
/// encoded preview from the entry's configured encoder.
#[derive(Serialize)]
struct LogEntrySample {
    id: String,
    name: String,
    rate: f64,
    /// Seconds between consecutive events (`1 / rate`).
    tick_secs: f64,
    /// Scenario duration in seconds, when bounded.
    duration_secs: Option<f64>,
    /// Start offset on the shared timeline in seconds.
    offset_secs: f64,
    /// Rendered log lines, capped at [`LOG_LINES_CAP`].
    lines: Vec<LogLineOut>,
    encoded_preview: String,
    labels: BTreeMap<String, String>,
}

/// One rendered log line. `severity` serializes lowercase (`"info"`).
#[derive(Serialize)]
struct LogLineOut {
    /// Seconds since scenario start on the shared timeline.
    secs: f64,
    severity: Severity,
    message: String,
}

/// One sampled histogram entry — per-tick, per-bucket observation counts
/// for a heatmap.
#[derive(Serialize)]
struct HistogramEntrySample {
    id: String,
    name: String,
    rate: f64,
    /// Seconds between consecutive ticks (`1 / rate`) for the x-axis.
    tick_secs: f64,
    /// Scenario duration in seconds, when bounded.
    duration_secs: Option<f64>,
    /// Start offset on the shared timeline in seconds.
    offset_secs: f64,
    /// Finite bucket upper bounds; every `counts` row carries one extra
    /// final cell for the `+Inf` bucket.
    bucket_bounds: Vec<f64>,
    /// `counts[tick][i]` — observations landing in bucket `i` during that
    /// tick (NOT cumulative, NOT `le`-style: each observation appears in
    /// exactly one cell, which is what a heatmap wants).
    counts: Vec<Vec<u64>>,
    labels: BTreeMap<String, String>,
}

/// One sampled summary entry — per-tick quantile values for band lines.
#[derive(Serialize)]
struct SummaryEntrySample {
    id: String,
    name: String,
    rate: f64,
    /// Seconds between consecutive ticks (`1 / rate`) for the x-axis.
    tick_secs: f64,
    /// Scenario duration in seconds, when bounded.
    duration_secs: Option<f64>,
    /// Start offset on the shared timeline in seconds.
    offset_secs: f64,
    /// Quantile targets, sorted ascending (e.g. `[0.5, 0.9, 0.95, 0.99]`).
    quantiles: Vec<f64>,
    /// `values[tick][i]` — the computed value for `quantiles[i]` at that
    /// tick.
    values: Vec<Vec<f64>>,
    labels: BTreeMap<String, String>,
}

/// One sampled metrics entry.
#[derive(Serialize)]
struct EntrySample {
    id: String,
    name: String,
    rate: f64,
    /// Seconds between consecutive ticks (`1 / rate`) for the x-axis.
    tick_secs: f64,
    /// Scenario duration in seconds, when bounded.
    duration_secs: Option<f64>,
    /// Start offset on the shared timeline in seconds. Non-zero when the
    /// compiler resolved an `after:` chain or the user set `phase_offset`.
    offset_secs: f64,
    /// Upstream entry id this entry's `after:` clause resolved against.
    after_ref: Option<String>,
    /// Human-readable `while:` gate description, when present.
    while_label: Option<String>,
    values: Vec<f64>,
    labels: BTreeMap<String, String>,
    gap: Option<GapOut>,
    burst: Option<BurstOut>,
    encoded_preview: String,
}

/// Gap window in seconds, for shading on the chart.
#[derive(Serialize)]
struct GapOut {
    every_secs: f64,
    for_secs: f64,
}

/// Burst window in seconds, for shading on the chart.
#[derive(Serialize)]
struct BurstOut {
    every_secs: f64,
    for_secs: f64,
    multiplier: f64,
}

/// An entry the playground cannot sample in the browser, with the reason.
#[derive(Serialize)]
struct SkippedEntry {
    id: String,
    reason: String,
}

/// Compile scenario YAML with the real sonda-core pipeline and sample every
/// metrics entry for up to `max_ticks` ticks.
///
/// Returns a JSON-encoded [`SampleResult`]. Never throws: compile and
/// per-entry errors are reported inside the JSON envelope.
#[wasm_bindgen]
pub fn sample_scenario(yaml: &str, max_ticks: u32) -> String {
    let result = run(yaml, max_ticks);
    serde_json::to_string(&result).unwrap_or_else(|err| {
        format!(
            "{{\"ok\":false,\"error\":\"internal serialization error: {}\",\"entries\":[],\"histograms\":[],\"summaries\":[],\"logs\":[],\"skipped\":[]}}",
            err.to_string().replace('"', "'")
        )
    })
}

fn run(yaml: &str, max_ticks: u32) -> SampleResult {
    // No pack resolution in the browser — `pack:` references report the
    // engine's own "unknown pack" error through the normal error path.
    let resolver = InMemoryPackResolver::default();
    let entries = match compile_scenario_file(yaml, &resolver) {
        Ok(entries) => entries,
        Err(err) => {
            return SampleResult {
                ok: false,
                error: Some(err.to_string()),
                entries: Vec::new(),
                histograms: Vec::new(),
                summaries: Vec::new(),
                logs: Vec::new(),
                skipped: Vec::new(),
            }
        }
    };

    // The compiled view of the same file carries what the runtime entries
    // fold away: the resolved `after:` upstream id and `while:` gates.
    // Entry order is preserved through every compile phase, so join by index.
    let compiled = compile_scenario_file_compiled(yaml, &resolver)
        .map(|file| file.entries)
        .unwrap_or_default();

    let mut sampled = Vec::new();
    let mut histograms = Vec::new();
    let mut summaries = Vec::new();
    let mut logs = Vec::new();
    let mut skipped = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        // Prefer the compiled entry's real id (e.g. "memory") — the runtime
        // entry only carries the metric name.
        let id = compiled
            .get(index)
            .and_then(|c| c.id.clone())
            .unwrap_or_else(|| entry_id(&entry, index));
        match desugar_entry(entry) {
            Ok(ScenarioEntry::Metrics(config)) => {
                match sample_metrics(&id, &config, compiled.get(index), max_ticks) {
                    Ok(sample) => sampled.push(sample),
                    Err(reason) => skipped.push(SkippedEntry { id, reason }),
                }
            }
            Ok(ScenarioEntry::Histogram(config)) => {
                match sample_histogram(&id, &config, compiled.get(index), max_ticks) {
                    Ok(sample) => histograms.push(sample),
                    Err(reason) => skipped.push(SkippedEntry { id, reason }),
                }
            }
            Ok(ScenarioEntry::Summary(config)) => {
                match sample_summary(&id, &config, compiled.get(index), max_ticks) {
                    Ok(sample) => summaries.push(sample),
                    Err(reason) => skipped.push(SkippedEntry { id, reason }),
                }
            }
            Ok(ScenarioEntry::Logs(config)) => {
                match sample_logs(&id, &config, compiled.get(index), max_ticks) {
                    Ok(sample) => logs.push(sample),
                    Err(reason) => skipped.push(SkippedEntry { id, reason }),
                }
            }
            // ScenarioEntry is non_exhaustive; future signal types surface
            // here as skipped rather than breaking the playground build.
            Ok(_) => skipped.push(SkippedEntry {
                id,
                reason: "this signal type is not visualized in the playground yet".into(),
            }),
            Err(err) => skipped.push(SkippedEntry {
                id,
                reason: err.to_string(),
            }),
        }
    }

    SampleResult {
        ok: true,
        error: None,
        entries: sampled,
        histograms,
        summaries,
        logs,
        skipped,
    }
}

/// Duration and resolved timeline offset shared by every entry kind.
fn entry_timing(
    base: &BaseScheduleConfig,
    compiled: Option<&CompiledEntry>,
) -> Result<(Option<f64>, f64), String> {
    let duration_secs = match base.duration.as_deref() {
        Some(s) => Some(parse_duration(s).map_err(|e| e.to_string())?.as_secs_f64()),
        None => None,
    };
    // The compiled entry's phase_offset is the resolved total (user offset
    // + after-chain crossing times + delays); fall back to the runtime
    // entry's own field when the compiled join is unavailable.
    let offset_secs = match compiled
        .and_then(|c| c.phase_offset.as_deref())
        .or(base.phase_offset.as_deref())
    {
        Some(s) => parse_duration(s).map_err(|e| e.to_string())?.as_secs_f64(),
        None => 0.0,
    };
    Ok((duration_secs, offset_secs))
}

/// Sample count bounded by the requested window and the scenario duration.
fn bounded_ticks(max_ticks: u32, cap: u32, rate: f64, duration_secs: Option<f64>) -> u64 {
    let mut ticks = max_ticks.clamp(2, cap) as u64;
    if let Some(secs) = duration_secs {
        let duration_ticks = (secs * rate).ceil() as u64;
        ticks = ticks.min(duration_ticks.max(2));
    }
    ticks
}

fn labels_map(base: &BaseScheduleConfig) -> BTreeMap<String, String> {
    base.labels
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn sample_histogram(
    id: &str,
    config: &HistogramScenarioConfig,
    compiled: Option<&CompiledEntry>,
    max_ticks: u32,
) -> Result<HistogramEntrySample, String> {
    // The facade bypasses prepare_entries, so run the same validation the
    // runtime would (sorted buckets, sane distribution) here.
    validate_histogram_config(config).map_err(|e| e.to_string())?;
    let mut generator = HistogramGenerator::from_config(config);
    let (duration_secs, offset_secs) = entry_timing(&config.base, compiled)?;
    // Heatmap payloads carry bucket_count cells per tick — cap the window
    // tighter than the line chart's.
    let ticks = bounded_ticks(max_ticks, 512, config.base.rate, duration_secs);
    let bucket_bounds = generator.buckets().to_vec();

    // The generator reports Prometheus-style cumulative `le` counts (across
    // ticks AND across the bucket chain). A heatmap wants each observation
    // exactly once: de-cumulate across ticks first, then along the chain,
    // with a final `+Inf` cell from the total count.
    let mut counts = Vec::with_capacity(ticks as usize);
    let mut prev_le: Vec<u64> = vec![0; bucket_bounds.len()];
    let mut prev_total: u64 = 0;
    for tick in 0..ticks {
        let sample = generator.observe(tick);
        let tick_le: Vec<u64> = sample
            .bucket_counts
            .iter()
            .zip(&prev_le)
            .map(|(now, before)| now.saturating_sub(*before))
            .collect();
        let tick_total = sample.count.saturating_sub(prev_total);
        let mut row = Vec::with_capacity(bucket_bounds.len() + 1);
        let mut below = 0u64;
        for &le in &tick_le {
            row.push(le.saturating_sub(below));
            below = le;
        }
        row.push(tick_total.saturating_sub(below));
        counts.push(row);
        prev_le = sample.bucket_counts;
        prev_total = sample.count;
    }

    Ok(HistogramEntrySample {
        id: id.to_string(),
        name: config.base.name.clone(),
        rate: config.base.rate,
        tick_secs: 1.0 / config.base.rate,
        duration_secs,
        offset_secs,
        bucket_bounds,
        counts,
        labels: labels_map(&config.base),
    })
}

fn sample_summary(
    id: &str,
    config: &SummaryScenarioConfig,
    compiled: Option<&CompiledEntry>,
    max_ticks: u32,
) -> Result<SummaryEntrySample, String> {
    validate_summary_config(config).map_err(|e| e.to_string())?;
    let mut generator = SummaryGenerator::from_config(config);
    let (duration_secs, offset_secs) = entry_timing(&config.base, compiled)?;
    let ticks = bounded_ticks(max_ticks, 1024, config.base.rate, duration_secs);
    let quantiles = generator.quantiles().to_vec();

    let mut values = Vec::with_capacity(ticks as usize);
    for tick in 0..ticks {
        let sample = generator.observe(tick);
        values.push(sample.quantiles.iter().map(|(_, value)| *value).collect());
    }

    Ok(SummaryEntrySample {
        id: id.to_string(),
        name: config.base.name.clone(),
        rate: config.base.rate,
        tick_secs: 1.0 / config.base.rate,
        duration_secs,
        offset_secs,
        quantiles,
        values,
        labels: labels_map(&config.base),
    })
}

fn entry_id(entry: &ScenarioEntry, index: usize) -> String {
    let name = entry.base().name.clone();
    if name.is_empty() {
        format!("entry[{index}]")
    } else {
        name
    }
}

fn sample_metrics(
    id: &str,
    config: &ScenarioConfig,
    compiled: Option<&CompiledEntry>,
    max_ticks: u32,
) -> Result<EntrySample, String> {
    let rate = config.base.rate;
    let generator = create_generator(&config.generator, rate).map_err(|e| e.to_string())?;
    let generator: Box<dyn ValueGenerator> = match config.base.jitter {
        Some(jitter) if jitter > 0.0 => Box::new(JitterWrapper::new(
            generator,
            jitter,
            config.base.jitter_seed.unwrap_or(0),
        )),
        _ => generator,
    };

    let (duration_secs, offset_secs) = entry_timing(&config.base, compiled)?;
    let after_ref = compiled.and_then(|c| c.after_ref.clone());
    let while_label = compiled.and_then(|c| {
        c.while_clause.as_ref().map(|w| {
            let op = match w.op {
                sonda_core::compiler::WhileOp::LessThan => "<",
                sonda_core::compiler::WhileOp::GreaterThan => ">",
            };
            format!("while {} {} {}", w.ref_id, op, w.value)
        })
    });

    let ticks = bounded_ticks(max_ticks, 4096, rate, duration_secs);
    let values: Vec<f64> = (0..ticks).map(|tick| generator.value(tick)).collect();
    let labels_map = labels_map(&config.base);

    let gap = match &config.base.gaps {
        Some(g) => Some(GapOut {
            every_secs: parse_duration(&g.every)
                .map_err(|e| e.to_string())?
                .as_secs_f64(),
            for_secs: parse_duration(&g.r#for)
                .map_err(|e| e.to_string())?
                .as_secs_f64(),
        }),
        None => None,
    };
    let burst = match &config.base.bursts {
        Some(b) => Some(BurstOut {
            every_secs: parse_duration(&b.every)
                .map_err(|e| e.to_string())?
                .as_secs_f64(),
            for_secs: parse_duration(&b.r#for)
                .map_err(|e| e.to_string())?
                .as_secs_f64(),
            multiplier: b.multiplier,
        }),
        None => None,
    };

    let encoded_preview = encode_preview(config, &values).unwrap_or_else(|reason| reason);

    Ok(EntrySample {
        id: id.to_string(),
        name: config.base.name.clone(),
        rate,
        tick_secs: 1.0 / rate,
        duration_secs,
        offset_secs,
        after_ref,
        while_label,
        values,
        labels: labels_map,
        gap,
        burst,
        encoded_preview,
    })
}

/// Sample a log entry: render the event stream via the clock-free
/// `LogGenerator::generate_at` path with timestamps synthesized from the
/// preview epoch, and encode the first few events with the entry's
/// configured encoder.
///
/// `csv_replay` needs the filesystem and is reported as skipped, the same
/// contract as the metric `csv_replay`.
fn sample_logs(
    id: &str,
    config: &LogScenarioConfig,
    compiled: Option<&CompiledEntry>,
    max_ticks: u32,
) -> Result<LogEntrySample, String> {
    if matches!(config.generator, LogGeneratorConfig::CsvReplay { .. }) {
        return Err(
            "log csv_replay reads a file — no filesystem in the browser; run it locally with `sonda run`"
                .into(),
        );
    }
    let generator = create_log_generator(&config.generator).map_err(|e| e.to_string())?;
    let encoder = create_encoder(&config.encoder).map_err(|e| e.to_string())?;
    let (duration_secs, offset_secs) = entry_timing(&config.base, compiled)?;
    let rate = config.base.rate;
    let ticks = bounded_ticks(max_ticks, LOG_LINES_CAP, rate, duration_secs);
    let tick_secs = 1.0 / rate;
    let labels = labels_map(&config.base);
    let pairs: Vec<(&str, &str)> = labels
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let event_labels = Labels::from_pairs(&pairs).map_err(|e| e.to_string())?;

    let mut lines = Vec::with_capacity(ticks as usize);
    let mut preview = Vec::new();
    for tick in 0..ticks {
        let secs = offset_secs + tick as f64 * tick_secs;
        let timestamp: SystemTime = UNIX_EPOCH
            + Duration::from_millis(PREVIEW_EPOCH_MS)
            + Duration::from_secs_f64(secs.max(0.0));
        let mut event = generator.generate_at(tick, timestamp);
        event.labels = event_labels.clone();
        if (tick as usize) < PREVIEW_EVENTS {
            // The runtime fails a log scenario whose encoder cannot encode
            // logs (e.g. prometheus_text) — surface the same error here.
            encoder
                .encode_log(&event, &mut preview)
                .map_err(|e| e.to_string())?;
        }
        lines.push(LogLineOut {
            secs,
            severity: event.severity,
            message: event.message,
        });
    }

    Ok(LogEntrySample {
        id: id.to_string(),
        name: config.base.name.clone(),
        rate,
        tick_secs,
        duration_secs,
        offset_secs,
        lines,
        encoded_preview: String::from_utf8_lossy(&preview).trim_end().to_string(),
        labels,
    })
}

/// Encode the first few sampled events with the entry's configured encoder.
///
/// Timestamps are synthesized from a fixed base — the browser sandbox has no
/// wall clock — spaced by the scenario rate.
fn encode_preview(config: &ScenarioConfig, values: &[f64]) -> Result<String, String> {
    let encoder = create_encoder(&config.encoder).map_err(|e| e.to_string())?;
    let name = ValidatedMetricName::new(&config.base.name).map_err(|e| e.to_string())?;
    let pairs: Vec<(&str, &str)> = config
        .base
        .labels
        .iter()
        .flatten()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let labels = std::sync::Arc::new(Labels::from_pairs(&pairs).map_err(|e| e.to_string())?);

    let interval_ms = (1000.0 / config.base.rate).max(1.0) as u64;
    let mut buf = Vec::new();
    for (i, &value) in values.iter().take(PREVIEW_EVENTS).enumerate() {
        let timestamp: SystemTime =
            UNIX_EPOCH + Duration::from_millis(PREVIEW_EPOCH_MS + i as u64 * interval_ms);
        let event = MetricEvent::from_parts(name.clone(), value, labels.clone(), timestamp);
        encoder
            .encode_metric(&event, &mut buf)
            .map_err(|e| e.to_string())?;
    }
    String::from_utf8(buf).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG_SCENARIO: &str = "\
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 30s
  encoder: { type: json_lines }
  sink: { type: stdout }
scenarios:
  - id: app
    signal_type: logs
    name: app_logs
    log_generator:
      type: template
      templates:
        - message: \"Request from {ip} to {endpoint}\"
          field_pools:
            ip: [\"10.0.0.1\", \"10.0.0.2\"]
            endpoint: [\"/api\", \"/health\"]
      severity_weights: { info: 0.7, warn: 0.2, error: 0.1 }
      seed: 7
    labels: { service: api }
";

    #[test]
    fn log_scenario_samples_lines_and_preview() {
        let json = sample_scenario(LOG_SCENARIO, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true, "log scenario must compile: {json}");
        assert!(
            parsed["skipped"].as_array().unwrap().is_empty(),
            "template logs must not be skipped anymore: {json}"
        );
        let log = &parsed["logs"][0];
        assert_eq!(log["id"], "app");
        assert_eq!(log["name"], "app_logs");
        let lines = log["lines"].as_array().unwrap();
        // rate 2 × 30s = 60 events.
        assert_eq!(lines.len(), 60);
        assert_eq!(lines[0]["secs"], 0.0);
        assert_eq!(lines[1]["secs"], 0.5);
        let message = lines[0]["message"].as_str().unwrap();
        assert!(
            message.starts_with("Request from"),
            "template resolved: {message}"
        );
        assert!(
            !message.contains('{'),
            "no unresolved placeholders: {message}"
        );
        let severity = lines[0]["severity"].as_str().unwrap();
        assert!(["trace", "debug", "info", "warn", "error", "fatal"].contains(&severity));
        // Encoded preview uses the json_lines encoder with the fixed epoch —
        // deterministic and label-carrying.
        let preview = log["encoded_preview"].as_str().unwrap();
        assert!(
            preview.contains("\"service\":\"api\""),
            "labels in preview: {preview}"
        );
        assert_eq!(preview.lines().count(), PREVIEW_EVENTS);
    }

    #[test]
    fn log_sampling_is_deterministic() {
        assert_eq!(
            sample_scenario(LOG_SCENARIO, 240),
            sample_scenario(LOG_SCENARIO, 240),
            "same YAML must produce byte-identical samples"
        );
    }

    #[test]
    fn log_csv_replay_is_skipped_with_a_reason() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 10s
  encoder: { type: json_lines }
  sink: { type: stdout }
scenarios:
  - id: replay
    signal_type: logs
    name: replayed
    log_generator: { type: csv_replay, file: events.csv }
";
        let json = sample_scenario(yaml, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert!(parsed["logs"].as_array().unwrap().is_empty());
        let reason = parsed["skipped"][0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("filesystem"),
            "reason explains the skip: {reason}"
        );
    }

    const SINE_SCENARIO: &str = "\
version: 2
kind: runnable
defaults:
  rate: 4
  duration: 30s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator: { type: sine, amplitude: 40.0, offset: 50.0, period_secs: 15 }
    labels: { host: web-01 }
";

    #[test]
    fn sine_scenario_samples_values_and_preview() {
        let json = sample_scenario(SINE_SCENARIO, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true);
        let entry = &parsed["entries"][0];
        assert_eq!(entry["name"], "cpu_usage");
        // duration 30s at rate 4 bounds the sample to 120 ticks.
        assert_eq!(entry["values"].as_array().map(Vec::len), Some(120));
        // Sine starts at its offset.
        assert_eq!(entry["values"][0], 50.0);
        let preview = entry["encoded_preview"].as_str().unwrap_or_default();
        assert!(preview.starts_with("cpu_usage{host=\"web-01\"} 50"));
    }

    #[test]
    fn alias_desugars_through_real_path() {
        let yaml = SINE_SCENARIO.replace(
            "{ type: sine, amplitude: 40.0, offset: 50.0, period_secs: 15 }",
            "{ type: flap, up_duration: 2s, down_duration: 1s }",
        );
        let json = sample_scenario(&yaml, 60);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true);
        let values = parsed["entries"][0]["values"].as_array().expect("values");
        // Flap at rate 4: 8 up ticks then 4 down ticks.
        assert_eq!(values[0], 1.0);
        assert_eq!(values[8], 0.0);
    }

    #[test]
    fn after_chain_produces_offset_and_ref() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 2
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: memory
    signal_type: metrics
    name: memory_percent
    duration: 120s
    generator: { type: leak, baseline: 10.0, ceiling: 95.0, time_to_ceiling: 120s }
  - id: latency
    signal_type: metrics
    name: latency_ms
    duration: 30s
    after: { ref: memory, op: '>', value: 40.0 }
    generator: { type: leak, baseline: 120.0, ceiling: 450.0, time_to_ceiling: 30s }
";
        let json = sample_scenario(yaml, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true, "compile failed: {:?}", parsed["error"]);
        let entries = parsed["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["id"], "memory");
        assert_eq!(entries[0]["offset_secs"], 0.0);
        assert_eq!(entries[1]["after_ref"], "memory");
        // leak 10→95 over 120s crosses 40 at ~42.4s.
        let offset = entries[1]["offset_secs"].as_f64().expect("offset");
        assert!(
            (41.0..44.0).contains(&offset),
            "expected crossing near 42s, got {offset}"
        );
    }

    #[test]
    fn histogram_entry_samples_a_heatmap_grid() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 10s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: latency
    signal_type: histogram
    name: http_request_duration_seconds
    distribution: { type: exponential, rate: 10.0 }
    observations_per_tick: 100
    seed: 42
    labels: { handler: /api }
";
        let json = sample_scenario(yaml, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true, "compile failed: {:?}", parsed["error"]);
        let histogram = &parsed["histograms"][0];
        assert_eq!(histogram["id"], "latency");
        // Default Prometheus buckets: 11 finite bounds.
        let bounds = histogram["bucket_bounds"].as_array().expect("bounds");
        assert_eq!(bounds.len(), 11);
        // 10s at rate 2 → 20 ticks; each row has one extra +Inf cell and
        // its cells sum to exactly observations_per_tick.
        let counts = histogram["counts"].as_array().expect("counts");
        assert_eq!(counts.len(), 20);
        for row in counts {
            let row = row.as_array().expect("row");
            assert_eq!(row.len(), 12);
            let total: u64 = row.iter().map(|c| c.as_u64().expect("cell")).sum();
            assert_eq!(total, 100, "each tick's cells must sum to obs/tick");
        }
    }

    #[test]
    fn summary_entry_samples_quantile_series() {
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 10s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: rpc
    signal_type: summary
    name: rpc_duration_seconds
    distribution: { type: normal, mean: 0.1, stddev: 0.02 }
    observations_per_tick: 100
    seed: 42
";
        let json = sample_scenario(yaml, 240);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true, "compile failed: {:?}", parsed["error"]);
        let summary = &parsed["summaries"][0];
        assert_eq!(summary["quantiles"].as_array().map(Vec::len), Some(4));
        let values = summary["values"].as_array().expect("values");
        assert_eq!(values.len(), 20);
        for row in values {
            let row = row.as_array().expect("row");
            assert_eq!(row.len(), 4);
            // Quantile values are sorted along the targets: p50 <= p99.
            let p50 = row[0].as_f64().expect("p50");
            let p99 = row[3].as_f64().expect("p99");
            assert!(p50 <= p99, "p50 {p50} must not exceed p99 {p99}");
        }
    }

    #[test]
    fn invalid_histogram_config_is_skipped_with_reason() {
        // Unsorted buckets never reach the generator — the same validation
        // the runtime runs reports through the skip channel.
        let yaml = "\
version: 2
kind: runnable
defaults:
  rate: 2
  duration: 10s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - id: bad
    signal_type: histogram
    name: broken_histogram
    buckets: [5.0, 1.0, 2.0]
    distribution: { type: exponential, rate: 10.0 }
";
        let json = sample_scenario(yaml, 60);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["histograms"].as_array().map(Vec::len), Some(0));
        assert_eq!(parsed["skipped"][0]["id"], "bad");
    }

    #[test]
    fn compile_error_is_reported_not_thrown() {
        let json = sample_scenario("version: 1\nscenarios: []\n", 60);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], false);
        assert!(parsed["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}
