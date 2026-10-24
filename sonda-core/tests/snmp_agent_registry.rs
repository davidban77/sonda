//! The SNMP agent registry binds nothing unless an `snmp_agent` sink is used.
//!
//! Lives in its own test binary because the registry is process-wide: lib
//! tests that bind agents in parallel would make an emptiness check, or a
//! count, racy.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(feature = "snmp")]

use std::sync::{Arc, RwLock};

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::snmp::{registry, SnmpAgentConfig, SnmpAgentSink};
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
        "a stdout scenario bound an snmp agent"
    );

    // Positive control: two sinks on one address are one live agent.
    let first = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
        .await
        .expect("bind an ephemeral port");
    let addr = first.agent().local_addr();
    let second = SnmpAgentSink::new(&SnmpAgentConfig::new(addr.to_string()))
        .await
        .expect("join the live agent");
    assert_eq!(second.agent().local_addr(), addr);
    assert!(Arc::ptr_eq(first.agent(), second.agent()));
    assert_eq!(registry().len(), 1);
    drop(first);
    assert_eq!(registry().len(), 1, "the second sink still holds the agent");
    drop(second);
    assert!(registry().is_empty(), "the last drop releases the agent");
}
