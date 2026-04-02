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
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "config", serde(tag = "type"))]
pub enum EncoderConfig {
    /// Prometheus text exposition format (version 0.0.4).
    ///
    /// `precision` optionally limits the number of decimal places in metric values.
    #[cfg_attr(feature = "config", serde(rename = "prometheus_text"))]
    PrometheusText {
        /// Maximum decimal places for metric values. `None` preserves full `f64` precision.
        #[cfg_attr(feature = "config", serde(default))]
        precision: Option<u8>,
    },
    /// InfluxDB line protocol.
    ///
    /// `field_key` sets the field key used for the metric value. Defaults to `"value"`.
    /// `precision` optionally limits the number of decimal places in metric values.
    #[cfg_attr(feature = "config", serde(rename = "influx_lp"))]
    InfluxLineProtocol {
        /// The InfluxDB field key for the metric value. Defaults to `"value"` if absent.
        field_key: Option<String>,
        /// Maximum decimal places for metric values. `None` preserves full `f64` precision.
        #[cfg_attr(feature = "config", serde(default))]
        precision: Option<u8>,
    },
    /// JSON Lines (NDJSON) format.
    ///
    /// Each event is serialized as one JSON object per line. Compatible with Elasticsearch,
    /// Loki, and generic HTTP ingest endpoints.
    ///
    /// `precision` optionally rounds the metric value before JSON serialization.
    #[cfg_attr(feature = "config", serde(rename = "json_lines"))]
    JsonLines {
        /// Maximum decimal places for metric values. `None` preserves full `f64` precision.
        #[cfg_attr(feature = "config", serde(default))]
        precision: Option<u8>,
    },
    /// RFC 5424 syslog format.
    ///
    /// Encodes log events as syslog lines. `hostname` and `app_name` default to `"sonda"`.
    #[cfg_attr(feature = "config", serde(rename = "syslog"))]
    Syslog {
        /// The HOSTNAME field in the syslog header. Defaults to `"sonda"`.
        hostname: Option<String>,
        /// The APP-NAME field in the syslog header. Defaults to `"sonda"`.
        app_name: Option<String>,
    },
    /// Prometheus remote write protobuf format.
    ///
    /// Encodes metric events as length-prefixed protobuf `TimeSeries` messages.
    /// Must be paired with the `remote_write` sink type, which batches TimeSeries
    /// into a single `WriteRequest`, snappy-compresses, and HTTP POSTs with the
    /// correct protocol headers. Requires the `remote-write` feature flag.
    #[cfg(feature = "remote-write")]
    #[cfg_attr(feature = "config", serde(rename = "remote_write"))]
    RemoteWrite,
}

/// Create a boxed [`Encoder`] from the given [`EncoderConfig`].
pub fn create_encoder(config: &EncoderConfig) -> Box<dyn Encoder> {
    match config {
        EncoderConfig::PrometheusText { precision } => {
            Box::new(prometheus::PrometheusText::new(*precision))
        }
        EncoderConfig::InfluxLineProtocol {
            field_key,
            precision,
        } => Box::new(influx::InfluxLineProtocol::new(
            field_key.clone(),
            *precision,
        )),
        EncoderConfig::JsonLines { precision } => Box::new(json::JsonLines::new(*precision)),
        EncoderConfig::Syslog { hostname, app_name } => {
            Box::new(syslog::Syslog::new(hostname.clone(), app_name.clone()))
        }
        #[cfg(feature = "remote-write")]
        EncoderConfig::RemoteWrite => Box::new(remote_write::RemoteWriteEncoder::new()),
    }
}

/// Write an f64 value to the buffer, optionally with fixed decimal precision.
///
/// When `precision` is `None`, uses Rust's default `Display` formatting for `f64`.
/// When `precision` is `Some(n)`, formats to exactly `n` decimal places.
pub(crate) fn write_value(buf: &mut Vec<u8>, value: f64, precision: Option<u8>) {
    use std::io::Write as _;
    match precision {
        None => write!(buf, "{}", value),
        Some(n) => write!(buf, "{:.1$}", value, n as usize),
    }
    .expect("write to Vec<u8> is infallible");
}

/// Fixed byte length of an RFC 3339 timestamp with millisecond precision.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.mmmZ` — always exactly 24 bytes.
pub(crate) const RFC3339_MILLIS_LEN: usize = 24;

/// Format a [`std::time::SystemTime`] as RFC 3339 with millisecond precision,
/// writing directly into the caller-provided buffer.
///
/// Appends exactly 24 bytes of the form `2026-03-20T12:00:00.000Z` to `buf`.
/// Computed entirely from `UNIX_EPOCH` arithmetic using the Gregorian calendar
/// algorithm from <https://howardhinnant.github.io/date_algorithms.html> — no
/// external crate required.
///
/// Returns a [`crate::SondaError::Encoder`] if the timestamp predates the Unix epoch.
pub(crate) fn format_rfc3339_millis(
    ts: std::time::SystemTime,
    buf: &mut Vec<u8>,
) -> Result<(), crate::SondaError> {
    let arr = format_rfc3339_millis_array(ts)?;
    buf.extend_from_slice(&arr);
    Ok(())
}

/// Format a [`std::time::SystemTime`] as RFC 3339 with millisecond precision
/// into a stack-allocated byte array.
///
/// Returns a fixed-size `[u8; 24]` containing valid UTF-8 of the form
/// `2026-03-20T12:00:00.000Z`. This avoids heap allocation entirely and is
/// suitable for callers that need a `&str` (e.g., serde serialization structs).
///
/// Returns a [`crate::SondaError::Encoder`] if the timestamp predates the Unix epoch.
pub(crate) fn format_rfc3339_millis_array(
    ts: std::time::SystemTime,
) -> Result<[u8; RFC3339_MILLIS_LEN], crate::SondaError> {
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

    let mut arr = [0u8; RFC3339_MILLIS_LEN];
    // write! into a &mut [u8] slice via std::io::Write.
    // The formatted output is always exactly 24 bytes, so this cannot fail.
    use std::io::Write as _;
    let mut cursor = &mut arr[..];
    write!(
        cursor,
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
    )
    .expect("RFC 3339 millis timestamp is always exactly 24 bytes");
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // EncoderConfig: internally-tagged deserialization (`type:` field)
    // These tests require the `config` feature (serde_yaml).
    // ---------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_prometheus_text_deserializes_with_type_field() {
        let yaml = "type: prometheus_text";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, EncoderConfig::PrometheusText { .. }));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_json_lines_deserializes_with_type_field() {
        let yaml = "type: json_lines";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, EncoderConfig::JsonLines { .. }));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_influx_lp_without_field_key_deserializes_with_type_field() {
        let yaml = "type: influx_lp";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol {
                field_key: None,
                precision: None
            }
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_influx_lp_with_field_key_deserializes_with_type_field() {
        let yaml = "type: influx_lp\nfield_key: requests";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol { field_key: Some(ref k), .. } if k == "requests"
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_unknown_type_returns_error() {
        let yaml = "type: no_such_encoder";
        let result: Result<EncoderConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "unknown type tag should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
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

    #[cfg(feature = "config")]
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
        let config = EncoderConfig::PrometheusText { precision: None };
        // If factory panics the test fails; just ensure it returns without error.
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_json_lines_succeeds() {
        let config = EncoderConfig::JsonLines { precision: None };
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_influx_lp_no_field_key_succeeds() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        };
        let _enc = create_encoder(&config);
    }

    #[test]
    fn create_encoder_influx_lp_with_field_key_succeeds() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("bytes".to_string()),
            precision: None,
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
        let config = EncoderConfig::PrometheusText { precision: None };
        let cloned = config.clone();
        assert!(matches!(cloned, EncoderConfig::PrometheusText { .. }));
        let s = format!("{config:?}");
        assert!(s.contains("PrometheusText"));
    }

    #[test]
    fn encoder_config_json_lines_is_cloneable_and_debuggable() {
        let config = EncoderConfig::JsonLines { precision: None };
        let cloned = config.clone();
        assert!(matches!(cloned, EncoderConfig::JsonLines { .. }));
        let s = format!("{config:?}");
        assert!(s.contains("JsonLines"));
    }

    #[test]
    fn encoder_config_influx_lp_is_cloneable_and_debuggable() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: Some("val".to_string()),
            precision: None,
        };
        let cloned = config.clone();
        assert!(matches!(
            cloned,
            EncoderConfig::InfluxLineProtocol { field_key: Some(ref k), .. } if k == "val"
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
            crate::model::metric::Labels::default(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn prometheus_encoder_encode_log_returns_not_supported_error() {
        let encoder = create_encoder(&EncoderConfig::PrometheusText { precision: None });
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
        let encoder = create_encoder(&EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        });
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
        let encoder = create_encoder(&EncoderConfig::JsonLines { precision: None });
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
        let encoder = create_encoder(&EncoderConfig::PrometheusText { precision: None });
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
        let encoder = create_encoder(&EncoderConfig::PrometheusText { precision: None });
        let event = make_log_event();
        let mut buf = Vec::new();
        let result = encoder.encode_log(&event, &mut buf);
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::SondaError::Encoder(_)),
            "error must be SondaError::Encoder variant, got: {err:?}"
        );
    }

    // ---------------------------------------------------------------------------
    // EncoderConfig::RemoteWrite (feature-gated tests)
    // ---------------------------------------------------------------------------

    #[cfg(all(feature = "remote-write", feature = "config"))]
    #[test]
    fn encoder_config_remote_write_deserializes_from_yaml() {
        let yaml = "type: remote_write";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            matches!(config, EncoderConfig::RemoteWrite),
            "should deserialize as RemoteWrite variant"
        );
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn create_encoder_remote_write_succeeds() {
        let config = EncoderConfig::RemoteWrite;
        let _enc = create_encoder(&config);
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn encoder_config_remote_write_is_cloneable_and_debuggable() {
        let config = EncoderConfig::RemoteWrite;
        let cloned = config.clone();
        assert!(matches!(cloned, EncoderConfig::RemoteWrite));
        let s = format!("{config:?}");
        assert!(
            s.contains("RemoteWrite"),
            "debug output should contain 'RemoteWrite', got: {s}"
        );
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn remote_write_encoder_produces_valid_output_through_factory() {
        use crate::model::metric::{Labels, MetricEvent};
        use std::time::{Duration, UNIX_EPOCH};

        let config = EncoderConfig::RemoteWrite;
        let enc = create_encoder(&config);

        let labels = Labels::from_pairs(&[("job", "sonda")]).unwrap();
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let event =
            MetricEvent::with_timestamp("factory_test".to_string(), 10.0, labels, ts).unwrap();

        let mut buf = Vec::new();
        enc.encode_metric(&event, &mut buf)
            .expect("encode through factory should succeed");
        assert!(
            !buf.is_empty(),
            "factory-created encoder should produce output"
        );
    }

    #[cfg(all(feature = "remote-write", feature = "config"))]
    #[test]
    fn scenario_yaml_with_remote_write_encoder_deserializes() {
        use crate::config::ScenarioConfig;
        use crate::sink::SinkConfig;

        let yaml = r#"
name: rw_test_metric
rate: 10.0
generator:
  type: constant
  value: 1.0
encoder:
  type: remote_write
sink:
  type: remote_write
  url: "http://localhost:8428/api/v1/write"
"#;
        let config: ScenarioConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "rw_test_metric");
        assert!(matches!(config.encoder, EncoderConfig::RemoteWrite));
        assert!(matches!(config.sink, SinkConfig::RemoteWrite { .. }));
    }

    // ---------------------------------------------------------------------------
    // write_value: shared helper for formatted f64 output
    // ---------------------------------------------------------------------------

    #[test]
    fn write_value_none_uses_default_display() {
        let mut buf = Vec::new();
        write_value(&mut buf, 1.0, None);
        assert_eq!(String::from_utf8(buf).unwrap(), "1");

        let mut buf = Vec::new();
        write_value(&mut buf, 3.14159, None);
        assert_eq!(String::from_utf8(buf).unwrap(), "3.14159");
    }

    #[test]
    fn write_value_precision_0() {
        let mut buf = Vec::new();
        write_value(&mut buf, 99.6, Some(0));
        assert_eq!(String::from_utf8(buf).unwrap(), "100");
    }

    #[test]
    fn write_value_precision_2() {
        let mut buf = Vec::new();
        write_value(&mut buf, 99.60573, Some(2));
        assert_eq!(String::from_utf8(buf).unwrap(), "99.61");

        let mut buf = Vec::new();
        write_value(&mut buf, 100.0, Some(2));
        assert_eq!(String::from_utf8(buf).unwrap(), "100.00");
    }

    #[test]
    fn write_value_precision_with_negative() {
        let mut buf = Vec::new();
        write_value(&mut buf, -3.14159, Some(2));
        assert_eq!(String::from_utf8(buf).unwrap(), "-3.14");
    }

    #[test]
    fn write_value_precision_4() {
        let mut buf = Vec::new();
        write_value(&mut buf, 1.23456789, Some(4));
        assert_eq!(String::from_utf8(buf).unwrap(), "1.2346");
    }

    // ---------------------------------------------------------------------------
    // EncoderConfig deserialization: precision field
    // These tests require the `config` feature (serde_yaml).
    // ---------------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn prometheus_text_with_precision_deserializes() {
        let yaml = "type: prometheus_text\nprecision: 3";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::PrometheusText { precision: Some(3) }
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn prometheus_text_without_precision_defaults_to_none() {
        let yaml = "type: prometheus_text";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::PrometheusText { precision: None }
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn influx_with_precision_and_field_key_deserializes() {
        let yaml = "type: influx_lp\nfield_key: gauge\nprecision: 2";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::InfluxLineProtocol {
                field_key: Some(ref k),
                precision: Some(2)
            } if k == "gauge"
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn json_lines_with_precision_deserializes() {
        let yaml = "type: json_lines\nprecision: 5";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::JsonLines { precision: Some(5) }
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn json_lines_without_precision_defaults_to_none() {
        let yaml = "type: json_lines";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::JsonLines { precision: None }
        ));
    }

    // ---------------------------------------------------------------------------
    // format_rfc3339_millis: buffer-based API
    // ---------------------------------------------------------------------------

    #[test]
    fn format_rfc3339_millis_writes_to_buffer() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let mut buf = Vec::new();
        format_rfc3339_millis(ts, &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "2026-03-20T12:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_appends_to_existing_buffer() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let mut buf = b"prefix:".to_vec();
        format_rfc3339_millis(ts, &mut buf).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "prefix:2026-03-20T12:00:00.000Z"
        );
    }

    #[test]
    fn format_rfc3339_millis_epoch_writes_correct_bytes() {
        use std::time::UNIX_EPOCH;
        let mut buf = Vec::new();
        format_rfc3339_millis(UNIX_EPOCH, &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_millis_before_epoch_returns_error() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH - Duration::from_secs(1);
        let mut buf = Vec::new();
        let result = format_rfc3339_millis(ts, &mut buf);
        assert!(result.is_err(), "timestamps before epoch must return error");
        assert!(
            buf.is_empty(),
            "buffer must remain empty on error (nothing written before failure)"
        );
    }

    // ---------------------------------------------------------------------------
    // format_rfc3339_millis_array: stack-allocated API
    // ---------------------------------------------------------------------------

    #[test]
    fn format_rfc3339_millis_array_returns_correct_bytes() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let arr = format_rfc3339_millis_array(ts).unwrap();
        assert_eq!(
            std::str::from_utf8(&arr).unwrap(),
            "2026-03-20T12:00:00.000Z"
        );
    }

    #[test]
    fn format_rfc3339_millis_array_epoch() {
        use std::time::UNIX_EPOCH;
        let arr = format_rfc3339_millis_array(UNIX_EPOCH).unwrap();
        assert_eq!(
            std::str::from_utf8(&arr).unwrap(),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn format_rfc3339_millis_array_before_epoch_returns_error() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH - Duration::from_secs(1);
        let result = format_rfc3339_millis_array(ts);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::SondaError::Encoder(_)),
            "error must be Encoder variant, got: {err:?}"
        );
    }

    #[test]
    fn format_rfc3339_millis_array_preserves_milliseconds() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_789);
        let arr = format_rfc3339_millis_array(ts).unwrap();
        let s = std::str::from_utf8(&arr).unwrap();
        assert!(s.ends_with(".789Z"), "must end with .789Z but got: {s}");
    }

    #[test]
    fn format_rfc3339_millis_array_and_buf_produce_identical_output() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let arr = format_rfc3339_millis_array(ts).unwrap();
        let mut buf = Vec::new();
        format_rfc3339_millis(ts, &mut buf).unwrap();
        assert_eq!(&arr[..], &buf[..]);
    }

    #[test]
    fn rfc3339_millis_len_constant_matches_output_size() {
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let mut buf = Vec::new();
        format_rfc3339_millis(ts, &mut buf).unwrap();
        assert_eq!(buf.len(), RFC3339_MILLIS_LEN);
    }
}
