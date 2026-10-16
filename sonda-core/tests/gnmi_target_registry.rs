//! The gNMI target registry binds nothing unless a `gnmi_target` sink is used.
//!
//! Lives in its own test binary because the registry is process-wide: lib
//! tests that bind targets in parallel would make an emptiness check racy.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(feature = "gnmi")]

mod common;

use std::sync::{Arc, RwLock};

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::gnmi::{registry, GnmiTargetConfig, GnmiTargetSink};
use sonda_core::sink::{create_sink, SinkConfig};
use sonda_core::{CancellationToken, OnSinkError};

#[tokio::test]
async fn unused_feature_binds_nothing() {
    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            gap_windows: None,
            name: "up".to_string(),
            rate: 100.0,
            duration: Some("50ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    };
    let mut sink = create_sink(&config.sink, None).await.expect("stdout sink");
    let stats = Arc::new(RwLock::new(ScenarioStats::default()));
    sonda_core::schedule::runner::run_with_sink(
        &config,
        &mut sink,
        &CancellationToken::new(),
        Some(Arc::clone(&stats)),
    )
    .await
    .expect("the stdout scenario runs");
    assert!(
        stats.read().unwrap().total_events > 0,
        "the scenario must have run for the check to mean anything"
    );
    assert!(
        registry().is_empty(),
        "a stdout scenario bound a gnmi target"
    );

    // Positive control: the registry does see a target while one is in use.
    let target_sink = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
        .await
        .expect("bind an ephemeral port");
    assert_eq!(registry().len(), 1);
    drop(target_sink);
    assert!(registry().is_empty(), "the last drop releases the target");
}
