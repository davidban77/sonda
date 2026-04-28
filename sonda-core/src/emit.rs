//! Synchronous single-event emission.
//!
//! Build a one-shot encoder + sink, encode one event, write, flush, drop.
//! I/O-agnostic — no latency measurement, no tracing.

use std::collections::HashMap;

use crate::encoder::{create_encoder, EncoderConfig};
use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::sink::{create_sink, SinkConfig};
use crate::SondaError;

/// Encode a single [`LogEvent`] and deliver it through a one-shot sink.
///
/// `labels` is forwarded to [`create_sink`]; use `None` when unused.
///
/// # Errors
///
/// Returns the underlying [`SondaError`] from any of the four steps:
/// encoder construction, sink construction, encoding, or sink
/// write/flush. Sink-side I/O failures surface as [`SondaError::Sink`];
/// invalid encoder/sink configs surface as [`SondaError::Config`].
pub fn emit_log(
    event: &LogEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    let encoder = create_encoder(encoder)?;
    let mut sink = create_sink(sink, labels)?;
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_log(event, &mut buf)?;
    sink.write(&buf)?;
    sink.flush()
}

/// Encode a single [`MetricEvent`] and deliver it through a one-shot sink.
///
/// `labels` is forwarded to [`create_sink`]; use `None` when unused.
///
/// # Errors
///
/// Returns the underlying [`SondaError`] from any of the four steps:
/// encoder construction, sink construction, encoding, or sink
/// write/flush. Sink-side I/O failures surface as [`SondaError::Sink`];
/// invalid encoder/sink configs surface as [`SondaError::Config`].
pub fn emit_metric(
    event: &MetricEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    let encoder = create_encoder(encoder)?;
    let mut sink = create_sink(sink, labels)?;
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_metric(event, &mut buf)?;
    sink.write(&buf)?;
    sink.flush()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::log::Severity;
    use crate::model::metric::Labels;
    use crate::sink::retry::RetryConfig;

    /// Per-test temp file path. Tagged with PID + thread id so parallel
    /// test runs (and reruns) do not collide on the same path.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "sonda-emit-{}-{:?}-{}.log",
            std::process::id(),
            std::thread::current().id(),
            tag,
        ));
        p
    }

    /// `emit_log` writes the encoded line through the constructed sink.
    ///
    /// The brief specifies `MemorySink` for this assertion, but the
    /// helpers take `&SinkConfig` and there is no public `Memory` variant
    /// in [`SinkConfig`]. The file sink is the closest stand-in that
    /// exercises the full helper end to end (encoder construction, sink
    /// construction, encode, write, flush) without reaching into private
    /// internals.
    #[test]
    fn emit_log_writes_encoded_line_to_sink() {
        let path = temp_path("emit_log_writes");
        let _ = std::fs::remove_file(&path);

        let event = LogEvent::new(
            Severity::Info,
            "hello from emit_log".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );

        emit_log(
            &event,
            &EncoderConfig::JsonLines { precision: None },
            &SinkConfig::File {
                path: path.to_string_lossy().into_owned(),
            },
            None,
        )
        .expect("emit_log must succeed");

        let contents = std::fs::read_to_string(&path).expect("read written file");
        let _ = std::fs::remove_file(&path);

        assert!(
            contents.contains("\"hello from emit_log\""),
            "encoded line must contain the message, got: {contents}"
        );
        assert!(
            contents.contains("\"severity\":\"info\""),
            "encoded line must contain severity, got: {contents}"
        );
        assert!(
            contents.ends_with('\n'),
            "JSON Lines output must end in a newline"
        );
    }

    /// `emit_metric` writes the encoded line through the constructed sink.
    #[test]
    fn emit_metric_writes_encoded_line_to_sink() {
        let path = temp_path("emit_metric_writes");
        let _ = std::fs::remove_file(&path);

        let event = MetricEvent::new(
            "deploy_event_total".to_string(),
            1.0,
            Labels::from_pairs(&[("event", "deploy_start")]).expect("labels"),
        )
        .expect("metric event");

        emit_metric(
            &event,
            &EncoderConfig::PrometheusText { precision: None },
            &SinkConfig::File {
                path: path.to_string_lossy().into_owned(),
            },
            None,
        )
        .expect("emit_metric must succeed");

        let contents = std::fs::read_to_string(&path).expect("read written file");
        let _ = std::fs::remove_file(&path);

        assert!(
            contents.contains("deploy_event_total"),
            "encoded line must contain the metric name, got: {contents}"
        );
        assert!(
            contents.contains("event=\"deploy_start\""),
            "encoded line must contain the label, got: {contents}"
        );
    }

    /// `emit_log` propagates `SondaError::Config` when the sink config is
    /// invalid (here: a TCP sink with `max_attempts = 0`, which the retry
    /// validator rejects).
    #[test]
    fn emit_log_propagates_config_error_for_invalid_sink_config() {
        let event = LogEvent::new(
            Severity::Info,
            "msg".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );

        let bad_sink = SinkConfig::Tcp {
            address: "127.0.0.1:1".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 0,
                initial_backoff: "100ms".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };

        let err = emit_log(
            &event,
            &EncoderConfig::JsonLines { precision: None },
            &bad_sink,
            None,
        )
        .expect_err("invalid retry config must fail sink construction");

        assert!(
            matches!(err, SondaError::Config(_)),
            "invalid retry config must surface as SondaError::Config, got: {err:?}"
        );
    }

    /// `emit_metric` propagates `SondaError::Config` when the sink config
    /// is invalid (here: a TCP sink with `max_attempts = 0`).
    #[test]
    fn emit_metric_propagates_config_error_for_invalid_sink_config() {
        let event =
            MetricEvent::new("test_metric".to_string(), 1.0, Labels::default()).expect("metric");

        let bad_sink = SinkConfig::Tcp {
            address: "127.0.0.1:1".to_string(),
            retry: Some(RetryConfig {
                max_attempts: 0,
                initial_backoff: "100ms".to_string(),
                max_backoff: "5s".to_string(),
            }),
        };

        let err = emit_metric(
            &event,
            &EncoderConfig::PrometheusText { precision: None },
            &bad_sink,
            None,
        )
        .expect_err("invalid retry config must fail sink construction");

        assert!(
            matches!(err, SondaError::Config(_)),
            "invalid retry config must surface as SondaError::Config, got: {err:?}"
        );
    }
}
