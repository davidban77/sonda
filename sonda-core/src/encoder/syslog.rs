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
