//! WebAssembly facade over the sonda-core engine.
//!
//! Powers the documentation-site playground: scenario YAML goes in, and the
//! real compiler + generators produce per-entry sampled values, schedule
//! windows, and an encoded output preview. Because this is the same code path
//! `sonda run` uses, the playground doubles as a validator — compile errors
//! come back with their real messages.
//!
//! Only the pure engine is linked (`sonda-core` without the `runtime`
//! feature), so the crate compiles to `wasm32-unknown-unknown`. Entries that
//! need a clock or the filesystem in the browser (logs, `csv_replay`) are
//! reported as skipped rather than sampled.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use wasm_bindgen::prelude::wasm_bindgen;

use sonda_core::compiler::expand::InMemoryPackResolver;
use sonda_core::config::validate::parse_duration;
use sonda_core::config::{ScenarioConfig, ScenarioEntry};
use sonda_core::encoder::create_encoder;
use sonda_core::generator::{create_generator, JitterWrapper, ValueGenerator};
use sonda_core::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use sonda_core::{compile_scenario_file, desugar_entry};

/// Fixed base timestamp for encoded previews (milliseconds since epoch).
///
/// The browser sandbox has no wall clock (`SystemTime::now()` panics on
/// `wasm32-unknown-unknown`), and a stable timestamp keeps preview output
/// deterministic for identical scenarios.
const PREVIEW_EPOCH_MS: u64 = 1_777_000_000_000;

/// How many encoded lines the preview pane shows per entry.
const PREVIEW_EVENTS: usize = 5;

/// Result envelope returned to JavaScript as JSON.
#[derive(Serialize)]
struct SampleResult {
    ok: bool,
    /// Compile error with the engine's real message, when `ok` is false.
    error: Option<String>,
    entries: Vec<EntrySample>,
    skipped: Vec<SkippedEntry>,
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
            "{{\"ok\":false,\"error\":\"internal serialization error: {}\",\"entries\":[],\"skipped\":[]}}",
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
                skipped: Vec::new(),
            }
        }
    };

    let mut sampled = Vec::new();
    let mut skipped = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let id = entry_id(&entry, index);
        match desugar_entry(entry) {
            Ok(ScenarioEntry::Metrics(config)) => match sample_metrics(&id, &config, max_ticks) {
                Ok(sample) => sampled.push(sample),
                Err(reason) => skipped.push(SkippedEntry { id, reason }),
            },
            Ok(ScenarioEntry::Logs(_)) => skipped.push(SkippedEntry {
                id,
                reason: "log entries are not visualized in the playground yet".into(),
            }),
            Ok(ScenarioEntry::Histogram(_)) => skipped.push(SkippedEntry {
                id,
                reason: "histogram entries are not visualized in the playground yet".into(),
            }),
            Ok(ScenarioEntry::Summary(_)) => skipped.push(SkippedEntry {
                id,
                reason: "summary entries are not visualized in the playground yet".into(),
            }),
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
        skipped,
    }
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

    let duration_secs = match config.base.duration.as_deref() {
        Some(s) => Some(parse_duration(s).map_err(|e| e.to_string())?.as_secs_f64()),
        None => None,
    };

    // Bound the sample count by the scenario duration when it is shorter
    // than the requested window.
    let mut ticks = max_ticks.clamp(2, 4096) as u64;
    if let Some(secs) = duration_secs {
        let duration_ticks = (secs * rate).ceil() as u64;
        ticks = ticks.min(duration_ticks.max(2));
    }

    let values: Vec<f64> = (0..ticks).map(|tick| generator.value(tick)).collect();

    let labels_map: BTreeMap<String, String> = config
        .base
        .labels
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

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
        values,
        labels: labels_map,
        gap,
        burst,
        encoded_preview,
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
    fn compile_error_is_reported_not_thrown() {
        let json = sample_scenario("version: 1\nscenarios: []\n", 60);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["ok"], false);
        assert!(parsed["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}
