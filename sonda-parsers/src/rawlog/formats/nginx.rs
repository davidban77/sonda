use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use sonda_core::Severity;

use crate::rawlog::{LogFormatParser, ParsedLogRow};

#[derive(Default)]
pub struct NginxParser {
    unrecognized_status_count: AtomicU64,
}

impl LogFormatParser for NginxParser {
    fn name(&self) -> &'static str {
        "nginx"
    }

    fn parse_line(&self, line: &str) -> Option<ParsedLogRow> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (row, unrecognized) = parse_combined(trimmed)?;
        if unrecognized {
            self.unrecognized_status_count
                .fetch_add(1, Ordering::Relaxed);
        }
        Some(row)
    }

    fn finalize(&self) {
        let count = self.unrecognized_status_count.load(Ordering::Relaxed);
        if count > 0 {
            tracing::warn!(
                count,
                "nginx parser saw {count} unrecognized HTTP status code(s); defaulted to info severity"
            );
        }
    }
}

fn parse_combined(line: &str) -> Option<(ParsedLogRow, bool)> {
    let bracket_start = line.find('[')?;
    let bracket_end = line[bracket_start + 1..].find(']')? + bracket_start + 1;
    let time_str = &line[bracket_start + 1..bracket_end];
    let timestamp = parse_nginx_time(time_str)?;

    let prefix = line[..bracket_start].trim_end();
    let remote_addr = prefix.split_whitespace().next()?.to_string();

    let after_bracket = line[bracket_end + 1..].trim_start();

    let (request, rest) = take_quoted(after_bracket)?;
    let rest = rest.trim_start();

    let mut tail = rest.split_whitespace();
    let status_str = tail.next()?;
    let _bytes = tail.next()?;

    let status: u16 = status_str.parse().ok()?;
    let (severity, recognized) = status_to_severity(status);

    let after_numbers = consume_two_tokens(rest)?;
    let (_referer, rest) = take_quoted(after_numbers.trim_start())?;
    let (user_agent, _) = take_quoted(rest.trim_start())?;

    let (method, path) = parse_request_line(&request);

    let message = format!("{request} {status_str}");

    let mut fields = BTreeMap::new();
    fields.insert("method".to_string(), method);
    fields.insert("path".to_string(), path);
    fields.insert("remote_addr".to_string(), remote_addr);
    fields.insert("status".to_string(), status_str.to_string());
    fields.insert("user_agent".to_string(), user_agent);

    Some((
        ParsedLogRow {
            timestamp: Some(timestamp),
            severity: Some(severity),
            message,
            fields,
        },
        !recognized,
    ))
}

fn parse_request_line(request: &str) -> (String, String) {
    let mut parts = request.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    (method, path)
}

fn take_quoted(s: &str) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    if chars.next().map(|(_, c)| c) != Some('"') {
        return None;
    }
    let mut out = String::new();
    let mut escape = false;
    for (idx, c) in chars {
        if escape {
            out.push(c);
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            return Some((out, &s[idx + c.len_utf8()..]));
        }
        out.push(c);
    }
    None
}

fn consume_two_tokens(s: &str) -> Option<&str> {
    let mut remaining = s;
    for _ in 0..2 {
        let after_ws = remaining.trim_start();
        let end = after_ws.find(char::is_whitespace).unwrap_or(after_ws.len());
        remaining = &after_ws[end..];
    }
    Some(remaining)
}

/// Returns `(severity, recognized)`. `recognized == false` flags the caller to
/// count this row for the aggregate end-of-run warn.
fn status_to_severity(status: u16) -> (Severity, bool) {
    match status {
        100..=399 => (Severity::Info, true),
        400..=499 => (Severity::Warn, true),
        500..=599 => (Severity::Error, true),
        _ => (Severity::Info, false),
    }
}

fn parse_nginx_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date_part, offset_part) = match s.find(' ') {
        Some(idx) => (&s[..idx], Some(s[idx + 1..].trim())),
        None => (s, None),
    };

    let mut chunks = date_part.split([':', '/']);
    let day = chunks.next()?.parse::<u32>().ok()?;
    let month = month_from_short_name(chunks.next()?)?;
    let year = chunks.next()?.parse::<i32>().ok()?;
    let hour = chunks.next()?.parse::<u32>().ok()?;
    let minute = chunks.next()?.parse::<u32>().ok()?;
    let second = chunks.next()?.parse::<u32>().ok()?;
    if chunks.next().is_some() {
        return None;
    }

    let utc_epoch = utc_epoch_from_components(year, month, day, hour, minute, second)?;
    let offset_seconds = match offset_part {
        Some(off) => parse_tz_offset_seconds(off)?,
        None => 0,
    };

    Some(utc_epoch as f64 - offset_seconds as f64)
}

fn month_from_short_name(s: &str) -> Option<u32> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_tz_offset_seconds(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign: i64 = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i64 = s[1..3].parse().ok()?;
    let minutes: i64 = s[3..5].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

fn utc_epoch_from_components(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !(1..=12).contains(&month) {
        return None;
    }
    let max_day = days_in_month(year, month);
    if !(1..=max_day).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month as i32, day as i32);
    let secs = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    Some(secs)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const SAMPLE_LINE: &str = r#"192.168.1.1 - alice [10/Oct/2024:13:55:36 +0000] "GET /api/v1/users HTTP/1.1" 200 1234 "-" "Mozilla/5.0""#;

    #[test]
    fn canonical_line_produces_row_with_timestamp_severity_message_fields() {
        let row = NginxParser::default().parse_line(SAMPLE_LINE).unwrap();
        assert_eq!(row.timestamp, Some(1_728_568_536.0));
        assert_eq!(row.severity, Some(Severity::Info));
        assert_eq!(row.message, r#"GET /api/v1/users HTTP/1.1 200"#);
        assert_eq!(row.fields.get("method").map(String::as_str), Some("GET"));
        assert_eq!(
            row.fields.get("path").map(String::as_str),
            Some("/api/v1/users")
        );
        assert_eq!(
            row.fields.get("remote_addr").map(String::as_str),
            Some("192.168.1.1")
        );
        assert_eq!(row.fields.get("status").map(String::as_str), Some("200"));
        assert_eq!(
            row.fields.get("user_agent").map(String::as_str),
            Some("Mozilla/5.0")
        );
    }

    #[rstest]
    #[case::ok(200, Severity::Info)]
    #[case::redirect(301, Severity::Info)]
    #[case::client_error(404, Severity::Warn)]
    #[case::forbidden(403, Severity::Warn)]
    #[case::server_error(500, Severity::Error)]
    #[case::gateway(502, Severity::Error)]
    fn status_maps_to_expected_severity(#[case] status: u16, #[case] expected: Severity) {
        let line = format!(
            r#"10.0.0.1 - - [10/Oct/2024:13:55:36 +0000] "GET / HTTP/1.1" {status} 0 "-" "ua""#,
        );
        let row = NginxParser::default().parse_line(&line).unwrap();
        assert_eq!(row.severity, Some(expected));
    }

    #[test]
    fn unrecognized_line_returns_none() {
        assert!(NginxParser::default()
            .parse_line("garbage line with no brackets")
            .is_none());
    }

    #[test]
    fn line_with_missing_quotes_returns_none() {
        assert!(NginxParser::default()
            .parse_line("10.0.0.1 - - [10/Oct/2024:13:55:36 +0000] GET 200 0 - ua")
            .is_none());
    }

    #[test]
    fn non_utc_offset_is_converted_to_epoch() {
        let line = r#"10.0.0.1 - - [10/Oct/2024:13:55:36 +0530] "GET / HTTP/1.1" 200 0 "-" "ua""#;
        let row = NginxParser::default().parse_line(line).unwrap();
        let expected_utc = 1_728_568_536.0 - (5.0 * 3600.0 + 30.0 * 60.0);
        assert_eq!(row.timestamp, Some(expected_utc));
    }

    #[test]
    fn negative_offset_is_converted_to_epoch() {
        let line = r#"10.0.0.1 - - [10/Oct/2024:13:55:36 -0800] "GET / HTTP/1.1" 200 0 "-" "ua""#;
        let row = NginxParser::default().parse_line(line).unwrap();
        let expected_utc = 1_728_568_536.0 + 8.0 * 3600.0;
        assert_eq!(row.timestamp, Some(expected_utc));
    }

    #[test]
    fn user_agent_with_spaces_is_captured_intact() {
        let line = r#"10.0.0.1 - - [10/Oct/2024:13:55:36 +0000] "GET / HTTP/1.1" 200 0 "-" "Mozilla/5.0 (X11; Linux x86_64) Chrome/100""#;
        let row = NginxParser::default().parse_line(line).unwrap();
        assert_eq!(
            row.fields.get("user_agent").map(String::as_str),
            Some("Mozilla/5.0 (X11; Linux x86_64) Chrome/100")
        );
    }

    #[test]
    fn name_is_nginx() {
        assert_eq!(NginxParser::default().name(), "nginx");
    }

    #[test]
    fn epoch_seconds_for_unix_anchor_match_known_value() {
        let line = r#"1.1.1.1 - - [01/Jan/1970:00:00:00 +0000] "GET / HTTP/1.1" 200 0 "-" "ua""#;
        let row = NginxParser::default().parse_line(line).unwrap();
        assert_eq!(row.timestamp, Some(0.0));
    }

    #[test]
    fn unrecognized_status_falls_back_to_info() {
        let line = r#"1.1.1.1 - - [10/Oct/2024:13:55:36 +0000] "GET / HTTP/1.1" 999 0 "-" "ua""#;
        let row = NginxParser::default().parse_line(line).unwrap();
        assert_eq!(row.severity, Some(Severity::Info));
    }
}
