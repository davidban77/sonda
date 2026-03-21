//! Syslog encoder (RFC 5424).
//!
//! Encodes [`LogEvent`]s in the RFC 5424 syslog format. Each encoded event is a single
//! line terminated with `\n`.
//!
//! RFC 5424 format:
//! ```text
//! <priority>VERSION TIMESTAMP HOSTNAME APP-NAME PROCID MSGID [STRUCTURED-DATA] MSG
//! ```
//!
//! Example output:
//! ```text
//! <14>1 2026-03-20T12:00:00.000Z sonda sonda - - - Request from 10.0.0.1\n
//! ```
//!
//! Priority is computed as `(facility * 8) + severity`. This encoder uses facility 1
//! (user-level messages) per RFC 5424 §6.2.1.
//!
//! The structured data section is always the nil value (`-`) — no structured data is
//! emitted. The PROCID and MSGID fields are also nil (`-`).
//!
//! No external crates are needed — the format is constructed entirely via `write!`
//! into the caller-provided buffer.

use std::time::UNIX_EPOCH;

use crate::model::log::{LogEvent, Severity};
use crate::model::metric::MetricEvent;
use crate::SondaError;

use super::Encoder;

/// RFC 5424 syslog facility for user-level messages.
const FACILITY_USER: u8 = 1;

/// RFC 5424 syslog version.
const SYSLOG_VERSION: u8 = 1;

/// Nil value for optional RFC 5424 header fields (PROCID, MSGID, SD).
const NILVALUE: &str = "-";

/// Maps a [`Severity`] to the corresponding RFC 5424 numeric severity code.
///
/// RFC 5424 §6.2.1 defines severity codes 0–7:
/// - 0 Emergency
/// - 1 Alert
/// - 2 Critical
/// - 3 Error
/// - 4 Warning
/// - 5 Notice
/// - 6 Informational
/// - 7 Debug
fn severity_to_syslog(severity: Severity) -> u8 {
    match severity {
        Severity::Fatal => 0, // Emergency
        Severity::Error => 3, // Error
        Severity::Warn => 4,  // Warning
        Severity::Info => 6,  // Informational
        Severity::Debug => 7, // Debug
        Severity::Trace => 7, // Debug (no finer-grained syslog severity)
    }
}

/// Formats a [`std::time::SystemTime`] as an RFC 3339 timestamp string with millisecond
/// precision suitable for use in a syslog header.
///
/// Returns an error if the timestamp predates the Unix epoch.
fn format_syslog_timestamp(ts: std::time::SystemTime) -> Result<String, SondaError> {
    let duration = ts
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SondaError::Encoder(format!("timestamp before Unix epoch: {e}")))?;

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

/// Encodes [`LogEvent`]s in RFC 5424 syslog format.
///
/// The hostname and app-name fields in the syslog header are configurable at construction
/// time. They default to `"sonda"` and `"sonda"` respectively.
///
/// Only `encode_log` is supported. `encode_metric` returns an error because syslog is a
/// log-only format.
pub struct Syslog {
    /// The HOSTNAME field in the syslog header.
    hostname: String,
    /// The APP-NAME field in the syslog header.
    app_name: String,
}

impl Syslog {
    /// Create a new `Syslog` encoder with the given hostname and app-name.
    ///
    /// # Arguments
    ///
    /// * `hostname` — The HOSTNAME field. Defaults to `"sonda"` if `None`.
    /// * `app_name` — The APP-NAME field. Defaults to `"sonda"` if `None`.
    pub fn new(hostname: Option<String>, app_name: Option<String>) -> Self {
        Self {
            hostname: hostname.unwrap_or_else(|| "sonda".to_string()),
            app_name: app_name.unwrap_or_else(|| "sonda".to_string()),
        }
    }
}

impl Default for Syslog {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl Encoder for Syslog {
    /// Syslog encodes only log events. Returns an error for metric events.
    fn encode_metric(
        &self,
        _event: &MetricEvent,
        _buf: &mut Vec<u8>,
    ) -> Result<(), crate::SondaError> {
        Err(SondaError::Encoder(
            "metric encoding not supported by syslog encoder".into(),
        ))
    }

    /// Encode a log event as an RFC 5424 syslog line appended to `buf`.
    ///
    /// Format: `<priority>1 timestamp hostname app-name - - - message\n`
    ///
    /// Priority = (facility * 8) + syslog_severity. Facility 1 (user-level) is used.
    fn encode_log(&self, event: &LogEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        let syslog_severity = severity_to_syslog(event.severity);
        let priority = FACILITY_USER * 8 + syslog_severity;
        let timestamp = format_syslog_timestamp(event.timestamp)?;

        // RFC 5424 HEADER: <PRI>VERSION SP TIMESTAMP SP HOSTNAME SP APP-NAME SP PROCID SP MSGID
        // RFC 5424 MSG: SP [STRUCTURED-DATA] SP MSG
        // We use nil values for PROCID, MSGID, and STRUCTURED-DATA.
        use std::io::Write;
        writeln!(
            buf,
            "<{priority}>{version} {timestamp} {hostname} {app_name} {procid} {msgid} {sd} {message}",
            priority = priority,
            version = SYSLOG_VERSION,
            timestamp = timestamp,
            hostname = self.hostname,
            app_name = self.app_name,
            procid = NILVALUE,
            msgid = NILVALUE,
            sd = NILVALUE,
            message = event.message,
        )
        .map_err(|e| SondaError::Encoder(format!("syslog format error: {e}")))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    use crate::model::log::{LogEvent, Severity};

    use super::*;

    /// Build a LogEvent with a fixed timestamp for deterministic tests.
    fn make_log_event(
        severity: Severity,
        message: &str,
        fields: &[(&str, &str)],
        ts: std::time::SystemTime,
    ) -> LogEvent {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v.to_string());
        }
        LogEvent::with_timestamp(ts, severity, message.to_string(), map)
    }

    // -----------------------------------------------------------------------
    // encode_metric: must return an error (syslog is log-only)
    // -----------------------------------------------------------------------

    #[test]
    fn encode_metric_returns_not_supported_error() {
        use crate::model::metric::{Labels, MetricEvent};
        let labels = Labels::from_pairs(&[]).unwrap();
        let event =
            MetricEvent::with_timestamp("cpu".to_string(), 1.0, labels, UNIX_EPOCH).unwrap();
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        let result = encoder.encode_metric(&event, &mut buf);
        assert!(
            result.is_err(),
            "syslog encoder must return error for encode_metric"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("metric encoding not supported"),
            "error message must mention 'metric encoding not supported', got: {msg}"
        );
    }

    #[test]
    fn encode_metric_does_not_write_to_buffer() {
        use crate::model::metric::{Labels, MetricEvent};
        let labels = Labels::from_pairs(&[]).unwrap();
        let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, UNIX_EPOCH).unwrap();
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        let _ = encoder.encode_metric(&event, &mut buf);
        assert!(
            buf.is_empty(),
            "buffer must remain empty when encode_metric returns error"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: happy path — valid RFC 5424 format
    // -----------------------------------------------------------------------

    #[test]
    fn encode_log_produces_line_ending_with_newline() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        assert_eq!(
            *buf.last().unwrap(),
            b'\n',
            "syslog line must end with newline"
        );
    }

    #[test]
    fn encode_log_starts_with_priority_marker() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.starts_with('<'),
            "syslog line must start with '<': {line}"
        );
    }

    #[test]
    fn encode_log_contains_version_one() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "test", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // After the priority, the next token is the version number
        let after_priority = line.find('>').unwrap();
        let version_token: &str = line[after_priority + 1..]
            .split_whitespace()
            .next()
            .unwrap();
        assert_eq!(version_token, "1", "RFC 5424 version must be 1");
    }

    #[test]
    fn encode_log_contains_hostname_in_output() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::new(Some("myhost".to_string()), None);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("myhost"),
            "syslog line must contain hostname 'myhost': {line}"
        );
    }

    #[test]
    fn encode_log_contains_app_name_in_output() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::new(None, Some("myapp".to_string()));
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("myapp"),
            "syslog line must contain app-name 'myapp': {line}"
        );
    }

    #[test]
    fn encode_log_default_hostname_and_app_name_are_sonda() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // "sonda sonda" should appear as consecutive tokens (hostname app_name)
        assert!(
            line.contains("sonda sonda"),
            "default hostname and app_name must both be 'sonda': {line}"
        );
    }

    #[test]
    fn encode_log_contains_message_in_output() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "request completed", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("request completed"),
            "syslog line must contain the message: {line}"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: priority calculation — (facility * 8) + syslog_severity
    // Facility 1 (user-level). Facility bits = 1 * 8 = 8.
    // -----------------------------------------------------------------------

    fn extract_priority(buf: &[u8]) -> u8 {
        let line = std::str::from_utf8(buf).unwrap();
        let end = line.find('>').expect("syslog line must contain '>'");
        line[1..end]
            .parse::<u8>()
            .expect("priority must be a number")
    }

    #[test]
    fn priority_for_trace_is_facility_user_plus_debug_syslog_severity() {
        // Trace maps to Debug (7) in syslog. Facility 1: 1*8 + 7 = 15
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Trace, "trace msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 15,
            "Trace priority must be 15 (facility=1, severity=7)"
        );
    }

    #[test]
    fn priority_for_debug_is_facility_user_plus_debug_syslog_severity() {
        // Debug maps to 7 in syslog. 1*8 + 7 = 15
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Debug, "debug msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 15,
            "Debug priority must be 15 (facility=1, severity=7)"
        );
    }

    #[test]
    fn priority_for_info_is_facility_user_plus_informational_syslog_severity() {
        // Info maps to 6 (Informational). 1*8 + 6 = 14
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "info msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 14,
            "Info priority must be 14 (facility=1, severity=6)"
        );
    }

    #[test]
    fn priority_for_warn_is_facility_user_plus_warning_syslog_severity() {
        // Warn maps to 4 (Warning). 1*8 + 4 = 12
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Warn, "warn msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 12,
            "Warn priority must be 12 (facility=1, severity=4)"
        );
    }

    #[test]
    fn priority_for_error_is_facility_user_plus_error_syslog_severity() {
        // Error maps to 3 (Error). 1*8 + 3 = 11
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Error, "error msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 11,
            "Error priority must be 11 (facility=1, severity=3)"
        );
    }

    #[test]
    fn priority_for_fatal_is_facility_user_plus_emergency_syslog_severity() {
        // Fatal maps to 0 (Emergency). 1*8 + 0 = 8
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Fatal, "fatal msg", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let priority = extract_priority(&buf);
        assert_eq!(
            priority, 8,
            "Fatal priority must be 8 (facility=1, severity=0)"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: RFC 5424 format structure — nil values for PROCID, MSGID, SD
    // -----------------------------------------------------------------------

    #[test]
    fn encode_log_contains_nil_values_for_procid_msgid_and_sd() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // After the header fields we expect three consecutive nil values (- - -)
        assert!(
            line.contains("- - -"),
            "syslog line must contain '- - -' (PROCID MSGID SD): {line}"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: timestamp format — RFC 3339 with millisecond precision
    // -----------------------------------------------------------------------

    #[test]
    fn encode_log_timestamp_is_rfc3339_with_millisecond_precision() {
        // 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "hello", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("2026-03-20T12:00:00.000Z"),
            "syslog line must contain RFC 3339 timestamp: {line}"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: message with special characters
    // -----------------------------------------------------------------------

    #[test]
    fn encode_log_message_with_spaces_is_included_verbatim() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(
            Severity::Info,
            "Request from 10.0.0.1 to /api/v2/metrics",
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("Request from 10.0.0.1 to /api/v2/metrics"),
            "message with spaces must be preserved: {line}"
        );
    }

    #[test]
    fn encode_log_message_with_unicode_characters() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Warn, "Ошибка: сервер недоступен", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("Ошибка: сервер недоступен"),
            "unicode message must be preserved: {line}"
        );
    }

    #[test]
    fn encode_log_message_with_angle_brackets() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Error, "value <nil> detected", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("value <nil> detected"),
            "message with angle brackets must be preserved: {line}"
        );
    }

    // -----------------------------------------------------------------------
    // encode_log: regression anchor — exact byte output
    // -----------------------------------------------------------------------

    #[test]
    fn regression_anchor_info_severity_exact_output() {
        // Timestamp: 2026-03-20T12:00:00.000Z = 1774008000 Unix seconds
        // Severity::Info -> syslog 6, priority = 1*8 + 6 = 14
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "Request from 10.0.0.1", &[], ts);
        let encoder = Syslog::new(Some("sonda".to_string()), Some("sonda".to_string()));
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "<14>1 2026-03-20T12:00:00.000Z sonda sonda - - - Request from 10.0.0.1\n"
        );
    }

    #[test]
    fn regression_anchor_error_severity_exact_output() {
        // Severity::Error -> syslog 3, priority = 1*8 + 3 = 11
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Error, "connection refused", &[], ts);
        let encoder = Syslog::new(Some("web01".to_string()), Some("nginx".to_string()));
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "<11>1 2026-03-20T12:00:00.000Z web01 nginx - - - connection refused\n"
        );
    }

    #[test]
    fn regression_anchor_fatal_severity_exact_output() {
        // Severity::Fatal -> syslog 0 (Emergency), priority = 1*8 + 0 = 8
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Fatal, "system crash", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "<8>1 2026-03-20T12:00:00.000Z sonda sonda - - - system crash\n"
        );
    }

    // -----------------------------------------------------------------------
    // Send + Sync contract
    // -----------------------------------------------------------------------

    #[test]
    fn syslog_encoder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Syslog>();
    }

    // -----------------------------------------------------------------------
    // EncoderConfig::Syslog: deserialization and factory wiring
    // -----------------------------------------------------------------------

    #[test]
    fn encoder_config_syslog_deserializes_without_optional_fields() {
        use crate::encoder::{create_encoder, EncoderConfig};
        let yaml = "type: syslog";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(
            matches!(
                config,
                EncoderConfig::Syslog {
                    hostname: None,
                    app_name: None
                }
            ),
            "syslog config without optional fields should have None for hostname and app_name"
        );
        // Also verify it can create an encoder
        let _enc = create_encoder(&config);
    }

    #[test]
    fn encoder_config_syslog_deserializes_with_hostname() {
        use crate::encoder::EncoderConfig;
        let yaml = "type: syslog\nhostname: myhost";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::Syslog {
                hostname: Some(ref h),
                app_name: None,
            } if h == "myhost"
        ));
    }

    #[test]
    fn encoder_config_syslog_deserializes_with_both_hostname_and_app_name() {
        use crate::encoder::EncoderConfig;
        let yaml = "type: syslog\nhostname: prod-01\napp_name: api-server";
        let config: EncoderConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::Syslog {
                hostname: Some(ref h),
                app_name: Some(ref a),
            } if h == "prod-01" && a == "api-server"
        ));
    }

    #[test]
    fn create_encoder_syslog_via_factory_encodes_log_event() {
        use crate::encoder::{create_encoder, EncoderConfig};
        let config = EncoderConfig::Syslog {
            hostname: Some("testhost".to_string()),
            app_name: Some("testapp".to_string()),
        };
        let encoder = create_encoder(&config);
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "factory test", &[], ts);
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("testhost"),
            "factory-created encoder must use configured hostname"
        );
        assert!(
            output.contains("testapp"),
            "factory-created encoder must use configured app_name"
        );
        assert!(
            output.contains("factory test"),
            "factory-created encoder must include the message"
        );
    }
}
