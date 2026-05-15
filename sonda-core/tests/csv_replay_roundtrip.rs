//! Round-trip integration test for the `csv_replay` fidelity feature.
//!
//! The flow synthesizes a CSV with a known sine pattern, runs it through
//! `expand_scenario`, instantiates the generator from the expanded config,
//! and asserts that the replayed values and derived emission rate match the
//! source data within numerical tolerance.

#![cfg(feature = "config")]

mod common;

use std::io::Write;

use sonda_core::config::{expand_scenario, BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{create_generator, GeneratorConfig};
use sonda_core::sink::SinkConfig;
use sonda_core::OnSinkError;

fn make_sine_csv(samples: usize, step_secs: u64) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
    writeln!(tmp, "Time,sine_value").expect("write header");
    let start_ts: u64 = 1_700_000_000;
    for i in 0..samples {
        let ts = start_ts + (i as u64) * step_secs;
        let theta = (i as f64) * std::f64::consts::TAU / (samples as f64);
        let value = theta.sin() * 10.0 + 50.0;
        writeln!(tmp, "{ts},{:.6}", value).expect("write row");
    }
    tmp.flush().expect("flush");
    tmp
}

fn build_scenario(file: String, timescale: Option<f64>) -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name: "roundtrip".to_string(),
            rate: 1.0,
            duration: Some("60s".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            dynamic_labels: None,
            on_sink_error: OnSinkError::Warn,
        },
        generator: GeneratorConfig::CsvReplay {
            file,
            column: None,
            repeat: Some(true),
            columns: None,
            timescale,
            default_metric_name: None,
        },
        encoder: EncoderConfig::PrometheusText { precision: None },
    }
}

#[test]
fn round_trip_sine_replay_matches_source_values_at_timescale_one() {
    const SAMPLES: usize = 60;
    const STEP_SECS: u64 = 10;
    let tmp = make_sine_csv(SAMPLES, STEP_SECS);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_scenario(path.clone(), Some(1.0));
    let expanded = expand_scenario(config).expect("expand must succeed");
    assert_eq!(expanded.len(), 1);
    let child = &expanded[0];

    let expected_rate = 1.0 / (STEP_SECS as f64);
    assert!(
        (child.rate - expected_rate).abs() < 1e-9,
        "derived rate must equal 1/Δt: expected {}, got {}",
        expected_rate,
        child.rate
    );

    let gen =
        create_generator(&child.generator, child.rate).expect("create_generator must succeed");
    let expected: Vec<f64> = (0..SAMPLES)
        .map(|i| {
            let theta = (i as f64) * std::f64::consts::TAU / (SAMPLES as f64);
            theta.sin() * 10.0 + 50.0
        })
        .collect();
    for (i, want) in expected.iter().enumerate() {
        let got = gen.value(i as u64);
        let want_rounded: f64 = format!("{:.6}", want).parse().unwrap();
        assert!(
            (got - want_rounded).abs() < 1e-9,
            "tick {}: expected ~{}, got {}",
            i,
            want_rounded,
            got
        );
    }
}

#[test]
fn round_trip_with_timescale_two_doubles_emission_rate() {
    let tmp = make_sine_csv(10, 10);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_scenario(path, Some(2.0));
    let expanded = expand_scenario(config).expect("expand must succeed");
    assert!(
        (expanded[0].rate - 0.2).abs() < 1e-9,
        "timescale=2.0 must double rate to 0.2, got {}",
        expanded[0].rate
    );
}

#[test]
fn round_trip_with_timescale_half_halves_emission_rate() {
    let tmp = make_sine_csv(10, 10);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_scenario(path, Some(0.5));
    let expanded = expand_scenario(config).expect("expand must succeed");
    assert!(
        (expanded[0].rate - 0.05).abs() < 1e-9,
        "timescale=0.5 must halve rate to 0.05, got {}",
        expanded[0].rate
    );
}

#[test]
fn round_trip_preserves_value_count_and_ordering() {
    let tmp = make_sine_csv(8, 5);
    let path = tmp.path().to_string_lossy().into_owned();

    let config = build_scenario(path, None);
    let expanded = expand_scenario(config).expect("expand must succeed");
    let gen = create_generator(&expanded[0].generator, expanded[0].rate)
        .expect("create_generator must succeed");

    let v0 = gen.value(0);
    let v1 = gen.value(1);
    let v_wrap = gen.value(8);
    assert!(
        (v_wrap - v0).abs() < 1e-12,
        "with repeat=true, tick 8 must wrap to value at tick 0: {v_wrap} vs {v0}"
    );
    assert_ne!(v0, v1, "consecutive samples must differ");
}
