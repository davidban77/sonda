//! Encoders serialize telemetry events into wire format bytes.
//!
//! All encoders implement the `Encoder` trait. They write into a caller-provided
//! `Vec<u8>` to avoid per-event allocations.

pub mod influx;
pub mod json;
pub mod prometheus;
#[cfg(feature = "remote-write")]
pub mod remote_write;
pub mod syslog;

use serde::Deserialize;

use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;

/// Encodes telemetry events into a specific wire format.
///
/// Implementations should pre-build any invariant content (label prefixes,
/// metric name validation) at construction time.
pub trait Encoder: Send + Sync {
    /// Encode a metric event into the provided buffer.
    fn encode_metric(
        &self,
        event: &MetricEvent,
        buf: &mut Vec<u8>,
    ) -> Result<(), crate::SondaError>;

    /// Encode a log event into the provided buffer.
    ///
    /// Returns an error by default. Encoders that support log encoding must
    /// override this method.
    fn encode_log(&self, _event: &LogEvent, _buf: &mut Vec<u8>) -> Result<(), crate::SondaError> {
        Err(crate::SondaError::Encoder(
            "log encoding not supported by this encoder".into(),
        ))
    }
}

/// Configuration selecting which encoder to use for a scenario.
///
/// This enum is serde-deserializable from YAML scenario files.
/// The `type` field selects the variant: `prometheus_text`, `influx_lp`, `json_lines`, or `syslog`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum EncoderConfig {
    /// Prometheus text exposition format (version 0.0.4).
    #[serde(rename = "prometheus_text")]
    PrometheusText,
    /// InfluxDB line protocol.
    ///
    /// `field_key` sets the field key used for the metric value. Defaults to `"value"`.
    #[serde(rename = "influx_lp")]
    InfluxLineProtocol {
        /// The InfluxDB field key for the metric value. Defaults to `"value"` if absent.
        field_key: Option<String>,
    },
    /// JSON Lines (NDJSON) format.
    ///
    /// Each event is serialized as one JSON object per line. Compatible with Elasticsearch,
    /// Loki, and generic HTTP ingest endpoints.
    #[serde(rename = "json_lines")]
    JsonLines,
    /// RFC 5424 syslog format.
    ///
    /// Encodes log events as syslog lines. `hostname` and `app_name` default to `"sonda"`.
    #[serde(rename = "syslog")]
    Syslog {
        /// The HOSTNAME field in the syslog header. Defaults to `"sonda"`.
        hostname: Option<String>,
        /// The APP-NAME field in the syslog header. Defaults to `"sonda"`.
        app_name: Option<String>,
    },
    /// Prometheus remote write protobuf format.
    ///
    /// Encodes metric events as Snappy-compressed protobuf `WriteRequest` messages
    /// compatible with the Prometheus remote write protocol. Requires the
    /// `remote-write` feature flag.
    ///
    /// The HTTP push sink should be configured with these headers:
    /// - `Content-Type: application/x-protobuf`
    /// - `Content-Encoding: snappy`
    /// - `X-Prometheus-Remote-Write-Version: 0.1.0`
    #[cfg(feature = "remote-write")]
    #[serde(rename = "remote_write")]
    RemoteWrite,
}

/// Create a boxed [`Encoder`] from the given [`EncoderConfig`].
pub fn create_encoder(config: &EncoderConfig) -> Box<dyn Encoder> {
    match config {
        EncoderConfig::PrometheusText => Box::new(prometheus::PrometheusText::new()),
        EncoderConfig::InfluxLineProtocol { field_key } => {
            Box::new(influx::InfluxLineProtocol::new(field_key.clone()))
        }
        EncoderConfig::JsonLines => Box::new(json::JsonLines::new()),
        EncoderConfig::Syslog { hostname, app_name } => {
            Box::new(syslog::Syslog::new(hostname.clone(), app_name.clone()))
        }
        #[cfg(feature = "remote-write")]
        EncoderConfig::RemoteWrite => Box::new(remote_write::RemoteWriteEncoder::new()),
    }
}

/// Format a [`std::time::SystemTime`] as an RFC 3339 string with millisecond precision.
///
/// Produces strings of the form `2026-03-20T12:00:00.000Z`. Computed entirely from
/// `UNIX_EPOCH` arithmetic using the Gregorian calendar algorithm from
/// <https://howardhinnant.github.io/date_algorithms.html> — no external crate required.
///
/// Returns a [`crate::SondaError::Encoder`] if the timestamp predates the Unix epoch.
pub(crate) fn format_rfc3339_millis(
    ts: std::time::SystemTime,
) -> Result<String, crate::SondaError> {
    use std::time::UNIX_EPOCH;

    let duration = ts
        .duration_since(UNIX_EPOCH)
        .map_err(|e| crate::SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?;

    let total_secs = duration.as_secs();
    let millis = duration.subsec_millis();

    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;

    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // civil_from_days: converts days since Unix epoch to (year, month, day).
    // Algorithm: https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // EncoderConfig: internally-tagged deserialization (`type:` field)
    // ---------------------------------------------------------------------------

    #[test]
    fn encoder_config_prometheus_text_deserializes_with_type_field() {
        let yaml = "type: prometheus_text";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, EncoderConfig::PrometheusText));
    }

    #[test]
    fn encoder_config_json_lines_deserializes_with_type_field() {
        let yaml = "type: json_lines";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, EncoderConfig::JsonLines));
    }

    #[test]
    fn encoder_config_influx_lp_without_field_key_deserializes_with_type_field() {
        let yaml = "type: influx_lp";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol { field_key: None }
        ));
    }

    #[test]
    fn encoder_config_influx_lp_with_field_key_deserializes_with_type_field() {
        let yaml = "type: influx_lp\nfield_key: requests";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol { field_key: Some(ref k) } if k == "requests"
        ));
    }

    #[test]
    fn encoder_config_unknown_type_returns_error() {
        let yaml = "type: no_such_encoder";
        let result: Result<EncoderConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown type tag should fail deserialization"
        );
    }

    #[test]
    fn encoder_config_missing_type_field_returns_error() {
        // Without the `type` field the internally-tagged enum cannot identify the variant.
        let yaml = "prometheus_text";
        let result: Result<EncoderConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "missing type field should fail deserialization"
        );
    }

    #[test]
    fn encoder_config_old_external_tag_format_is_rejected() {
        // The old externally-tagged format (`!prometheus_text`) must no longer be accepted.
        let yaml = "!prometheus_text";
        let result: Result<EncoderConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "externally-tagged YAML format must be rejected in favour of internally-tagged"
        );
    }

    // ---------------------------------------------------------------------------
    // EncoderConfig: factory wiring for all variants
    // ---------------------------------------------------------------------------

    #[test]
    fn create_encoder_prometheus_text_succeeds() {
        let config = EncoderConfig::PrometheusText;
        // If factory panics the test fails; just ensure it returns without error.
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_json_lines_succeeds() {
        let config = EncoderConfig::JsonLines;
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_influx_lp_no_field_key_succeeds() {
        let config = EncoderConfig::InfluxLineProtocol { field_key: None };
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_influx_lp_with_field_key_succeeds() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("bytes".to_string()),
        };
        let _enc = create_encoder(&config);
    }

    // ---------------------------------------------------------------------------
    // EncoderConfig: Send + Sync contract
    // ---------------------------------------------------------------------------

    #[test]
    fn encoder_config_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EncoderConfig>();
    }

    // ---------------------------------------------------------------------------
    // EncoderConfig: Clone + Debug contract
    // ---------------------------------------------------------------------------

    #[test]
    fn encoder_config_prometheus_text_is_cloneable_and_debuggable() {
        let config = EncoderConfig::PrometheusText;
        let cloned = config.clone();
        assert!(matches!(cloned, EncoderConfig::PrometheusText));
        let s = format!("{config:?}");
        assert!(s.contains("PrometheusText"));
    }

    #[test]
    fn encoder_config_json_lines_is_cloneable_and_debuggable() {
        let config = EncoderConfig::JsonLines;
        let cloned = config.clone();
        assert!(matches!(cloned, EncoderConfig::JsonLines));
        let s = format!("{config:?}");
        assert!(s.contains("JsonLines"));
    }

    #[test]
    fn encoder_config_influx_lp_is_cloneable_and_debuggable() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("val".to_string()),
        };
        let cloned = config.clone();
        assert!(matches!(
            cloned,
            EncoderConfig::InfluxLineProtocol { field_key: Some(ref k) } if k == "val"
        ));
        let s = format!("{config:?}");
        assert!(s.contains("InfluxLineProtocol"));
    }

    // ---------------------------------------------------------------------------
    // Encoder trait: default encode_log() returns "not supported" error
    // ---------------------------------------------------------------------------

    fn make_log_event() -> crate::model::log::LogEvent {
        use std::collections::BTreeMap;
        crate::model::log::LogEvent::new(
            crate::model::log::Severity::Info,
            "test message".to_string(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn prometheus_encoder_encode_log_returns_not_supported_error() {
        let encoder = create_encoder(&EncoderConfig::PrometheusText);
        let event = make_log_event();
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        assert!(
            result.is_err(),
            "prometheus encoder must return an error for encode_log"
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not supported"),
            "error message should contain 'not supported', got: {msg}"
        );
    }

    #[test]
    fn influx_encoder_encode_log_returns_not_supported_error() {
        let encoder = create_encoder(&EncoderConfig::InfluxLineProtocol { field_key: None });
        let event = make_log_event();
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        assert!(
            result.is_err(),
            "influx encoder must return an error for encode_log"
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not supported"),
            "error message should contain 'not supported', got: {msg}"
        );
    }

    #[test]
    fn json_lines_encoder_encode_log_succeeds() {
        // Slice 2.3: JsonLines now implements encode_log — it must succeed, not return an error.
        let encoder = create_encoder(&EncoderConfig::JsonLines);
        let event = make_log_event();
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        assert!(
            result.is_ok(),
            "json_lines encoder must support encode_log after slice 2.3"
        );
        assert!(!buf.is_empty(), "buffer must contain encoded data");
    }

    #[test]
    fn encode_log_default_does_not_write_to_buffer() {
        // The default implementation must not produce partial output in the buffer.
        let encoder = create_encoder(&EncoderConfig::PrometheusText);
        let event = make_log_event();
        let mut buf = Vec::new();
        let _ = encoder.encode_log(&event, &mut buf);
        assert!(
            buf.is_empty(),
            "buffer must remain empty when encode_log returns an error"
        );
    }

    #[test]
    fn encode_log_error_is_encoder_variant() {
        // The error must come back as SondaError::Encoder, not some other variant.
        let encoder = create_encoder(&EncoderConfig::PrometheusText);
        let event = make_log_event();
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::SondaError::Encoder(_)),
            "error must be SondaError::Encoder variant, got: {err:?}"
        );
    }
}
