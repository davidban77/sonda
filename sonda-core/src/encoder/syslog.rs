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
//! Example output (no labels):
//! ```text
//! <14>1 2026-03-20T12:00:00.000Z sonda sonda - - - Request from 10.0.0.1\n
//! ```
//!
//! Example output (with labels):
//! ```text
//! <14>1 2026-03-20T12:00:00.000Z sonda sonda - - [sonda device="wlan0" hostname="router-01"] Request from 10.0.0.1\n
//! ```
//!
//! Priority is computed as `(facility * 8) + severity`. This encoder uses facility 1
//! (user-level messages) per RFC 5424 §6.2.1.
//!
//! When labels are present, the structured data section contains a `[sonda ...]`
//! element with label key-value pairs. When labels are empty, the structured data
//! section is the nil value (`-`). The PROCID and MSGID fields are always nil (`-`).
//!
//! Param-values are escaped per RFC 5424 §6.3.3: `\`, `]`, and `"` are prefixed
//! with a backslash.
//!
//! No external crates are needed — the format is constructed entirely via `write!`
//! into the caller-provided buffer.

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
    /// Format (no labels): `<priority>1 timestamp hostname app-name - - - message\n`
    /// Format (with labels): `<priority>1 timestamp hostname app-name - - [sonda k="v" ...] message\n`
    ///
    /// Priority = (facility * 8) + syslog_severity. Facility 1 (user-level) is used.
    fn encode_log(&self, event: &LogEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
        use std::io::Write;

        let syslog_severity = severity_to_syslog(event.severity);
        let priority = FACILITY_USER * 8 + syslog_severity;

        // Write the priority, version, and space before timestamp.
        write!(buf, "<{priority}>{version} ", version = SYSLOG_VERSION)
            .expect("write to Vec<u8> is infallible");

        // Write the RFC 3339 timestamp directly into the output buffer (zero-alloc).
        super::format_rfc3339_millis(event.timestamp, buf)?;

        // Write the rest of the header fields.
        write!(
            buf,
            " {hostname} {app_name} {procid} {msgid} ",
            hostname = self.hostname,
            app_name = self.app_name,
            procid = NILVALUE,
            msgid = NILVALUE,
        )
        .expect("write to Vec<u8> is infallible");

        // Write structured data section: nil when no labels, [sonda k="v" ...]
        // when labels are present.
        if event.labels.is_empty() {
            buf.extend_from_slice(NILVALUE.as_bytes());
        } else {
            buf.extend_from_slice(b"[sonda");
            for (k, v) in event.labels.iter() {
                buf.push(b' ');
                buf.extend_from_slice(k.as_bytes());
                buf.extend_from_slice(b"=\"");
                // Escape \, ], " per RFC 5424 §6.3.3
                for ch in v.bytes() {
                    match ch {
                        b'\\' => buf.extend_from_slice(b"\\\\"),
                        b']' => buf.extend_from_slice(b"\\]"),
                        b'"' => buf.extend_from_slice(b"\\\""),
                        _ => buf.push(ch),
                    }
                }
                buf.push(b'"');
            }
            buf.push(b']');
        }

        // Write message and trailing newline.
        buf.push(b' ');
        buf.extend_from_slice(event.message.as_bytes());
        buf.push(b'\n');

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
        LogEvent::with_timestamp(
            ts,
            severity,
            message.to_string(),
            crate::model::metric::Labels::default(),
            map,
        )
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
    // encode_log: labels in structured data section
    // -----------------------------------------------------------------------

    /// Build a LogEvent with labels for testing structured data output.
    fn make_log_event_with_labels(
        severity: Severity,
        message: &str,
        labels: &[(&str, &str)],
        fields: &[(&str, &str)],
        ts: std::time::SystemTime,
    ) -> LogEvent {
        let mut field_map = BTreeMap::new();
        for (k, v) in fields {
            field_map.insert(k.to_string(), v.to_string());
        }
        let label_set = crate::model::metric::Labels::from_pairs(labels).unwrap();
        LogEvent::with_timestamp(ts, severity, message.to_string(), label_set, field_map)
    }

    #[test]
    fn encode_log_with_labels_includes_structured_data() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "labeled event",
            &[("device", "wlan0")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(
            line.contains("[sonda device=\"wlan0\"]"),
            "syslog line must contain structured data [sonda device=\"wlan0\"]: {line}"
        );
    }

    #[test]
    fn encode_log_with_multiple_labels_includes_all_in_structured_data() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "multi-label event",
            &[("device", "wlan0"), ("hostname", "router_01")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // Labels are sorted by key (BTreeMap), so device comes before hostname
        assert!(
            line.contains("[sonda device=\"wlan0\" hostname=\"router_01\"]"),
            "syslog line must contain sorted labels in structured data: {line}"
        );
    }

    #[test]
    fn encode_log_without_labels_uses_nil_structured_data() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event(Severity::Info, "no labels", &[], ts);
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // Without labels, SD should be nil: "- - -" pattern (PROCID MSGID SD)
        assert!(
            line.contains("- - -"),
            "syslog line without labels must use nil SD (- - -): {line}"
        );
        assert!(
            !line.contains("[sonda"),
            "syslog line without labels must not contain [sonda: {line}"
        );
    }

    #[test]
    fn encode_log_with_labels_escapes_backslash_in_value() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "escape test",
            &[("path", "C:\\Users\\admin")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // Backslashes in values must be escaped to \\ per RFC 5424 §6.3.3
        assert!(
            line.contains("path=\"C:\\\\Users\\\\admin\""),
            "backslashes in label values must be escaped: {line}"
        );
    }

    #[test]
    fn encode_log_with_labels_escapes_closing_bracket_in_value() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "bracket test",
            &[("tag", "foo]bar")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // Closing brackets must be escaped to \] per RFC 5424 §6.3.3
        assert!(
            line.contains("tag=\"foo\\]bar\""),
            "closing bracket in label value must be escaped: {line}"
        );
    }

    #[test]
    fn encode_log_with_labels_escapes_double_quote_in_value() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "quote test",
            &[("desc", "it said \"hello\"")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // Double quotes must be escaped to \" per RFC 5424 §6.3.3
        assert!(
            line.contains("desc=\"it said \\\"hello\\\"\""),
            "double quotes in label value must be escaped: {line}"
        );
    }

    #[test]
    fn encode_log_with_labels_escapes_all_special_characters_combined() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "combined escape",
            &[("mixed", "a\\b]c\"d")],
            &[],
            ts,
        );
        let encoder = Syslog::default();
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        // All three special chars must be escaped
        assert!(
            line.contains("mixed=\"a\\\\b\\]c\\\"d\""),
            "all special characters must be escaped: {line}"
        );
    }

    #[test]
    fn regression_anchor_info_severity_with_labels_exact_output() {
        let ts = UNIX_EPOCH + Duration::from_millis(1_774_008_000_000);
        let event = make_log_event_with_labels(
            Severity::Info,
            "Request from 10.0.0.1",
            &[("device", "wlan0"), ("hostname", "router_01")],
            &[],
            ts,
        );
        let encoder = Syslog::new(Some("sonda".to_string()), Some("sonda".to_string()));
        let mut buf = Vec::new();
        encoder.encode_log(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "<14>1 2026-03-20T12:00:00.000Z sonda sonda - - [sonda device=\"wlan0\" hostname=\"router_01\"] Request from 10.0.0.1\n"
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

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_syslog_deserializes_without_optional_fields() {
        use crate::encoder::{create_encoder, EncoderConfig};
        let yaml = "type: syslog";
        let config: EncoderConfig = serde_yaml_ng::from_str(yaml).unwrap();
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

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_syslog_deserializes_with_hostname() {
        use crate::encoder::EncoderConfig;
        let yaml = "type: syslog\nhostname: myhost";
        let config: EncoderConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(
            config,
            EncoderConfig::Syslog {
                hostname: Some(ref h),
                app_name: None,
            } if h == "myhost"
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn encoder_config_syslog_deserializes_with_both_hostname_and_app_name() {
        use crate::encoder::EncoderConfig;
        let yaml = "type: syslog\nhostname: prod-01\napp_name: api-server";
        let config: EncoderConfig = serde_yaml_ng::from_str(yaml).unwrap();
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
