//! Round-trip integration tests for the `log_csv_replay` generator.

#![cfg(feature = "config")]

mod common;

use std::collections::BTreeMap;
use std::io::Write;

use sonda_core::config::{expand_log_scenario, BaseScheduleConfig, LogScenarioConfig};
use sonda_core::encoder::{create_encoder, EncoderConfig};
use sonda_core::generator::{create_log_generator, LogGeneratorConfig};
use sonda_core::model::log::Severity;
use sonda_core::sink::SinkConfig;
use sonda_core::OnSinkError;

fn make_log_csv(rows: &[(&str, &str, &str, &str)], step_secs: u64) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    writeln!(tmp, "timestamp,severity,message,user_id").expect("write header");
    let start_ts: u64 = 1_700_000_000;
    for (i, (sev, msg, _ts_ignored, user_id)) in rows.iter().enumerate() {
        let ts = start_ts + (i as u64) * step_secs;
        writeln!(tmp, "{ts},{sev},{msg},{user_id}").expect("write row");
    }
    tmp.flush().expect("flush");
    tmp
}

fn build_log_scenario(file: String, timescale: Option<f64>) -> LogScenarioConfig {
    LogScenarioConfig {
        base: BaseScheduleConfig {
            name: "log_roundtrip".to_string(),
            rate: 1.0,
            duration: Some("60s".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Warn,
        },
        generator: LogGeneratorConfig::CsvReplay {
            file,
            columns: None,
            repeat: Some(true),
            timescale,
            default_severity: None,
        },
        encoder: EncoderConfig::JsonLines { precision: None },
    }
}

#[test]
fn round_trip_emits_one_log_event_per_source_row_within_20_ticks() {
    let rows = vec![
        ("info", "row-zero", "", "u0"),
        ("warn", "row-one", "", "u1"),
        ("error", "row-two", "", "u2"),
        ("info", "row-three", "", "u3"),
        ("info", "row-four", "", "u4"),
    ];
    let tmp = make_log_csv(&rows, 10);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_log_scenario(path, Some(1.0));
    let expanded = expand_log_scenario(config).expect("expand must succeed");
    assert_eq!(expanded.len(), 1);
    let child = &expanded[0];

    let expected_rate = 1.0 / 10.0;
    assert!(
        (child.base.rate - expected_rate).abs() < 1e-9,
        "derived rate must equal 1/Δt: expected {expected_rate}, got {}",
        child.base.rate
    );

    let gen = create_log_generator(&child.generator).expect("create_log_generator must succeed");

    let expected_severities = [
        Severity::Info,
        Severity::Warn,
        Severity::Error,
        Severity::Info,
        Severity::Info,
    ];
    for tick in 0..20u64 {
        let i = (tick as usize) % rows.len();
        let event = gen.generate(tick);
        assert_eq!(
            event.message, rows[i].1,
            "tick {tick}: expected message {}, got {}",
            rows[i].1, event.message
        );
        assert_eq!(
            event.severity, expected_severities[i],
            "tick {tick}: severity mismatch"
        );
        assert_eq!(
            event.fields.get("user_id").map(String::as_str),
            Some(rows[i].3),
            "tick {tick}: user_id field mismatch"
        );
    }
}

#[test]
fn round_trip_json_lines_encoder_produces_parseable_output() {
    let rows = vec![
        ("info", "first event", "", "alice"),
        ("warn", "second event", "", "bob"),
    ];
    let tmp = make_log_csv(&rows, 5);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_log_scenario(path, None);
    let expanded = expand_log_scenario(config).expect("expand must succeed");
    let child = &expanded[0];

    let gen = create_log_generator(&child.generator).expect("create_log_generator must succeed");
    let encoder = create_encoder(&child.encoder).expect("create_encoder must succeed");

    for tick in 0..rows.len() {
        let event = gen.generate(tick as u64);
        let mut buf: Vec<u8> = Vec::with_capacity(256);
        encoder
            .encode_log(&event, &mut buf)
            .expect("encode_log must succeed");
        let s = std::str::from_utf8(&buf).expect("encoded JSON line must be UTF-8");
        let trimmed = s.trim_end_matches('\n');
        let parsed: serde_json::Value =
            serde_json::from_str(trimmed).expect("encoded line must be valid JSON");
        let obj = parsed.as_object().expect("JSON must be an object");
        assert_eq!(
            obj.get("message").and_then(|v| v.as_str()),
            Some(rows[tick].1),
            "tick {tick}: encoded message mismatch"
        );
        let fields = obj
            .get("fields")
            .and_then(|v| v.as_object())
            .expect("encoded output must include fields");
        assert_eq!(
            fields.get("user_id").and_then(|v| v.as_str()),
            Some(rows[tick].3)
        );
    }
}

#[test]
fn round_trip_mixed_severities_preserved_across_multiple_cycles() {
    let rows = vec![
        ("info", "i0", "", "u0"),
        ("warn", "w0", "", "u1"),
        ("error", "e0", "", "u2"),
        ("info", "i1", "", "u3"),
    ];
    let tmp = make_log_csv(&rows, 5);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_log_scenario(path, None);
    let expanded = expand_log_scenario(config).expect("expand must succeed");
    let gen =
        create_log_generator(&expanded[0].generator).expect("create_log_generator must succeed");

    let mut counts: BTreeMap<Severity, usize> = BTreeMap::new();
    let total_ticks = rows.len() * 3;
    for tick in 0..total_ticks as u64 {
        *counts.entry(gen.generate(tick).severity).or_insert(0) += 1;
    }
    assert_eq!(counts.get(&Severity::Info).copied().unwrap_or(0), 6);
    assert_eq!(counts.get(&Severity::Warn).copied().unwrap_or(0), 3);
    assert_eq!(counts.get(&Severity::Error).copied().unwrap_or(0), 3);
}
