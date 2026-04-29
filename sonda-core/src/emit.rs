//! Synchronous single-event emission.

use std::collections::HashMap;

use crate::encoder::{create_encoder, EncoderConfig};
use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::sink::{create_sink, SinkConfig};
use crate::SondaError;

/// Encode a [`LogEvent`] and deliver it through a one-shot sink.
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

/// Encode a [`MetricEvent`] and deliver it through a one-shot sink.
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
