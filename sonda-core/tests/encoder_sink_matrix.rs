#![cfg(feature = "config")]
//! Encoder × Sink matrix integration tests.
//!
//! Three `#[rstest]` families (one per encoder) cover memory-shape checks,
//! file round-trips, TCP/UDP delivery, and stdout write-flush via the `Op`
//! discriminant. Encoder-unique assertions and regression anchors are
//! standalone tests. Feature-gated sinks (HTTP push, Kafka, OTLP) each have
//! a dedicated rstest fn or module.

#[cfg(feature = "http")]
use std::collections::HashMap;
use std::io::Read;
#[cfg(feature = "http")]
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::Path;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use rstest::rstest;

use sonda_core::encoder::{create_encoder, EncoderConfig};
use sonda_core::model::metric::{Labels, MetricEvent};
use sonda_core::sink::memory::MemorySink;
use sonda_core::sink::Sink;

fn test_event() -> MetricEvent {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1"), ("env", "test")]).unwrap();
    MetricEvent::with_timestamp("matrix_test_metric".to_string(), 42.0, labels, ts).unwrap()
}

fn test_event_no_labels() -> MetricEvent {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    MetricEvent::with_timestamp(
        "matrix_no_labels".to_string(),
        1.0,
        Labels::from_pairs(&[]).unwrap(),
        ts,
    )
    .unwrap()
}

fn encode_event(config: &EncoderConfig, event: &MetricEvent) -> Vec<u8> {
    let encoder = create_encoder(config).expect("encoder factory must succeed");
    let mut buf = Vec::new();
    encoder
        .encode_metric(event, &mut buf)
        .expect("encode must succeed");
    buf
}

fn tmp_matrix_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("sonda-matrix-tests")
        .join(format!("{name}-matrix.txt"))
}

// ---------------------------------------------------------------------------
// Op discriminant for per-encoder rstest families
// ---------------------------------------------------------------------------

/// Selects the assertion for a per-encoder rstest row.
#[derive(Debug, Clone, Copy)]
enum Op {
    MemoryNonempty,
    MemoryContainsIdentifier,
    MemoryEndsNewline,
    FileSink,
    TcpSink,
    UdpSink,
    StdoutSink,
}

fn check_memory_nonempty(bytes: &[u8], enc: &str) {
    let mut sink = MemorySink::new();
    sink.write(bytes).unwrap();
    sink.flush().unwrap();
    assert!(!sink.buffer.is_empty(), "{enc} encoder produced no output");
}

fn check_file_roundtrip(bytes: &[u8], tag: &str) {
    let path = tmp_matrix_path(tag);
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut sink = sonda_core::sink::file::FileSink::new(Path::new(&path)).unwrap();
    sink.write(bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

fn check_tcp_delivery(bytes: &[u8]) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let expected = bytes.to_vec();
    let rx = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        buf
    });
    let mut sink = sonda_core::sink::tcp::TcpSink::new(&addr, None).unwrap();
    sink.write(bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);
    assert_eq!(rx.join().unwrap(), expected);
}

fn check_udp_delivery(bytes: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    let mut sink = sonda_core::sink::udp::UdpSink::new(&addr).unwrap();
    sink.write(bytes).unwrap();
    let mut buf = vec![0u8; 4096];
    let (len, _) = socket.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..len], bytes);
}

fn check_stdout(bytes: &[u8]) {
    let mut sink = sonda_core::sink::stdout::StdoutSink::new();
    assert!(sink.write(bytes).is_ok());
    assert!(sink.flush().is_ok());
}

// ---------------------------------------------------------------------------
// Encoder families
// ---------------------------------------------------------------------------

/// Prometheus text encoder: memory-shape checks + all delivery sinks.
#[rstest]
#[rustfmt::skip]
#[case::memory_produces_nonempty_output(       Op::MemoryNonempty)]
#[case::memory_output_contains_metric_name(    Op::MemoryContainsIdentifier)]
#[case::memory_output_ends_with_newline(        Op::MemoryEndsNewline)]
#[case::file_write_and_read_back(              Op::FileSink)]
#[case::tcp_data_arrives_at_listener(          Op::TcpSink)]
#[case::udp_datagram_arrives_at_receiver(      Op::UdpSink)]
#[case::stdout_write_and_flush_succeed(        Op::StdoutSink)]
fn prometheus_encoder_cases(#[case] op: Op) {
    let config = EncoderConfig::PrometheusText { precision: None };
    let bytes = encode_event(&config, &test_event());
    match op {
        Op::MemoryNonempty           => check_memory_nonempty(&bytes, "prometheus"),
        Op::MemoryContainsIdentifier => assert!(std::str::from_utf8(&bytes).unwrap().contains("matrix_test_metric")),
        Op::MemoryEndsNewline        => assert_eq!(*bytes.last().unwrap(), b'\n'),
        Op::FileSink                 => check_file_roundtrip(&bytes, "prometheus"),
        Op::TcpSink                  => check_tcp_delivery(&bytes),
        Op::UdpSink                  => check_udp_delivery(&bytes),
        Op::StdoutSink               => check_stdout(&bytes),
    }
}

/// InfluxDB Line Protocol encoder: memory-shape checks + all delivery sinks.
#[rstest]
#[rustfmt::skip]
#[case::memory_produces_nonempty_output(       Op::MemoryNonempty)]
#[case::memory_output_contains_measurement(    Op::MemoryContainsIdentifier)]
#[case::memory_output_has_nanosecond_timestamp(Op::MemoryEndsNewline)]
#[case::file_write_and_read_back(              Op::FileSink)]
#[case::tcp_data_arrives_at_listener(          Op::TcpSink)]
#[case::udp_datagram_arrives_at_receiver(      Op::UdpSink)]
#[case::stdout_write_and_flush_succeed(        Op::StdoutSink)]
fn influx_encoder_cases(#[case] op: Op) {
    let config = EncoderConfig::InfluxLineProtocol { field_key: None, precision: None };
    let bytes = encode_event(&config, &test_event());
    match op {
        Op::MemoryNonempty           => check_memory_nonempty(&bytes, "influx"),
        Op::MemoryContainsIdentifier => assert!(std::str::from_utf8(&bytes).unwrap().contains("matrix_test_metric")),
        Op::MemoryEndsNewline => {
            let text = std::str::from_utf8(&bytes).unwrap().trim_end_matches('\n').to_owned();
            let ts = text.split_whitespace().last().unwrap();
            assert!(ts.len() >= 19, "influx timestamp must be >=19 digits (nanosecond precision): {ts}");
        }
        Op::FileSink   => check_file_roundtrip(&bytes, "influx"),
        Op::TcpSink    => check_tcp_delivery(&bytes),
        Op::UdpSink    => check_udp_delivery(&bytes),
        Op::StdoutSink => check_stdout(&bytes),
    }
}

/// JSON Lines encoder: memory-shape checks + all delivery sinks.
#[rstest]
#[rustfmt::skip]
#[case::memory_produces_nonempty_output(       Op::MemoryNonempty)]
#[case::memory_output_is_valid_json(           Op::MemoryContainsIdentifier)]
#[case::memory_output_ends_with_newline(        Op::MemoryEndsNewline)]
#[case::file_write_and_read_back(              Op::FileSink)]
#[case::tcp_data_arrives_at_listener(          Op::TcpSink)]
#[case::udp_datagram_arrives_at_receiver(      Op::UdpSink)]
#[case::stdout_write_and_flush_succeed(        Op::StdoutSink)]
fn json_encoder_cases(#[case] op: Op) {
    let config = EncoderConfig::JsonLines { precision: None };
    let bytes = encode_event(&config, &test_event());
    match op {
        Op::MemoryNonempty => check_memory_nonempty(&bytes, "json"),
        Op::MemoryContainsIdentifier => {
            let line = std::str::from_utf8(&bytes).unwrap().trim_end_matches('\n');
            assert!(serde_json::from_str::<serde_json::Value>(line).unwrap().is_object());
        }
        Op::MemoryEndsNewline => assert_eq!(*bytes.last().unwrap(), b'\n'),
        Op::FileSink   => check_file_roundtrip(&bytes, "json"),
        Op::TcpSink    => check_tcp_delivery(&bytes),
        Op::UdpSink    => check_udp_delivery(&bytes),
        Op::StdoutSink => check_stdout(&bytes),
    }
}

// ---------------------------------------------------------------------------
// Encoder-unique standalone assertions
// ---------------------------------------------------------------------------

#[test]
fn prometheus_x_memory_no_labels_omits_braces() {
    let bytes = encode_event(
        &EncoderConfig::PrometheusText { precision: None },
        &test_event_no_labels(),
    );
    assert!(!std::str::from_utf8(&bytes).unwrap().contains('{'));
}

#[test]
fn influx_x_memory_custom_field_key_appears_in_output() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: Some("requests".to_string()),
        precision: None,
    };
    assert!(std::str::from_utf8(&encode_event(&config, &test_event()))
        .unwrap()
        .contains("requests="));
}

#[test]
fn json_x_memory_output_contains_all_expected_fields() {
    let bytes = encode_event(&EncoderConfig::JsonLines { precision: None }, &test_event());
    let line = std::str::from_utf8(&bytes).unwrap().trim_end_matches('\n');
    let p: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(p["name"], "matrix_test_metric");
    assert!((p["value"].as_f64().unwrap() - 42.0).abs() < f64::EPSILON);
    assert_eq!(p["labels"]["host"], "srv1");
    assert_eq!(p["labels"]["env"], "test");
}

// ---------------------------------------------------------------------------
// HTTP push (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "http")]
fn http_listener_url() -> (TcpListener, String) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    (l, format!("http://127.0.0.1:{port}/push"))
}

#[cfg(feature = "http")]
fn accept_http_ok(listener: &TcpListener) -> Vec<u8> {
    let (mut s, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(s.try_clone().unwrap());
    let mut cl: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let low = line.to_lowercase();
        if low.starts_with("content-length:") {
            cl = low["content-length:".len()..].trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; cl];
    reader.read_exact(&mut body).unwrap();
    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .ok();
    body
}

#[cfg(feature = "http")]
#[rstest]
#[rustfmt::skip]
#[case::prometheus_http_push_body_matches(EncoderConfig::PrometheusText { precision: None },                            "text/plain; version=0.0.4")]
#[case::influx_http_push_body_matches(    EncoderConfig::InfluxLineProtocol { field_key: None, precision: None },       "text/plain")]
#[case::json_http_push_body_matches(      EncoderConfig::JsonLines { precision: None },                                 "application/x-ndjson")]
fn encoder_http_push_body_matches(#[case] config: EncoderConfig, #[case] ct: &str) {
    let (listener, url) = http_listener_url();
    let bytes = encode_event(&config, &test_event());
    let expected = bytes.clone();
    let server = thread::spawn(move || accept_http_ok(&listener));
    let mut sink =
        sonda_core::sink::http::HttpPushSink::new(&url, ct, 10_000, HashMap::new(), None, Duration::ZERO)
            .unwrap();
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    assert_eq!(server.join().unwrap(), expected);
}

// ---------------------------------------------------------------------------
// Kafka (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "kafka")]
mod kafka_matrix {
    use super::*;
    use sonda_core::SondaError;

    #[rstest]
    #[rustfmt::skip]
    #[case::prometheus(EncoderConfig::PrometheusText { precision: None })]
    #[case::influx(    EncoderConfig::InfluxLineProtocol { field_key: None, precision: None })]
    #[case::json(      EncoderConfig::JsonLines { precision: None })]
    fn kafka_encoder_bytes_are_nonempty(#[case] config: EncoderConfig) {
        assert!(!encode_event(&config, &test_event()).is_empty());
    }

    #[test]
    fn kafka_sink_with_empty_broker_returns_sink_error() {
        let cfg = sonda_core::sink::SinkConfig::Kafka {
            brokers: String::new(),
            topic: "sonda-test".to_string(),
            max_buffer_age: None,
            retry: None,
            tls: None,
            sasl: None,
        };
        let result = sonda_core::sink::create_sink(&cfg, None);
        let Err(e) = result else {
            panic!("expected Err from create_sink with empty broker, got Ok");
        };
        assert!(matches!(e, SondaError::Sink(_)));
    }
}

// ---------------------------------------------------------------------------
// OTLP (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "otlp")]
mod otlp_matrix {
    use super::*;
    use sonda_core::encoder::otlp::OtlpEncoder;
    use sonda_core::encoder::Encoder;
    use sonda_core::model::log::{LogEvent, Severity};
    use std::collections::BTreeMap;

    fn assert_lp(buf: &[u8]) {
        assert!(buf.len() >= 4);
        let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(buf.len(), 4 + n, "length prefix must match payload");
    }

    fn log_event(sev: Severity, msg: &str) -> LogEvent {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        LogEvent::with_timestamp(
            ts,
            sev,
            msg.to_owned(),
            Labels::from_pairs(&[("env", "test")]).unwrap(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn otlp_x_memory_metric_produces_nonempty_output() {
        let mut buf = Vec::new();
        OtlpEncoder::new()
            .encode_metric(&test_event(), &mut buf)
            .unwrap();
        let mut sink = MemorySink::new();
        sink.write(&buf).unwrap();
        sink.flush().unwrap();
        assert!(!sink.buffer.is_empty());
    }

    #[test]
    fn otlp_x_memory_metric_has_valid_length_prefix() {
        let mut buf = Vec::new();
        OtlpEncoder::new()
            .encode_metric(&test_event(), &mut buf)
            .unwrap();
        assert_lp(&buf);
    }

    #[test]
    fn otlp_x_memory_log_produces_nonempty_output() {
        let mut buf = Vec::new();
        OtlpEncoder::new()
            .encode_log(&log_event(Severity::Info, "msg"), &mut buf)
            .unwrap();
        let mut sink = MemorySink::new();
        sink.write(&buf).unwrap();
        sink.flush().unwrap();
        assert!(!sink.buffer.is_empty());
    }

    #[test]
    fn otlp_x_memory_log_has_valid_length_prefix() {
        let mut buf = Vec::new();
        OtlpEncoder::new()
            .encode_log(&log_event(Severity::Warn, "warn"), &mut buf)
            .unwrap();
        assert_lp(&buf);
    }

    #[test]
    fn otlp_x_memory_multi_metric_accumulates_in_sink() {
        let enc = OtlpEncoder::new();
        let mut sink = MemorySink::new();
        for i in 0..3u64 {
            let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + i * 1000);
            let labels = Labels::from_pairs(&[("idx", &i.to_string())]).unwrap();
            let event =
                MetricEvent::with_timestamp("otlp_multi".to_owned(), i as f64, labels, ts).unwrap();
            let mut buf = Vec::new();
            enc.encode_metric(&event, &mut buf).unwrap();
            sink.write(&buf).unwrap();
        }
        sink.flush().unwrap();
        assert!(sink.buffer.len() > 12);
    }
}

// ---------------------------------------------------------------------------
// Multi-event pipeline (one rstest fn, 3 cases)
// ---------------------------------------------------------------------------

/// Five events encoded in sequence produce correctly-named, valid lines.
#[rstest]
#[rustfmt::skip]
#[case::prometheus_multi_event_accumulates_lines(EncoderConfig::PrometheusText { precision: None },                      "multi_event",  false)]
#[case::influx_multi_event_accumulates_lines(    EncoderConfig::InfluxLineProtocol { field_key: None, precision: None }, "multi_influx", false)]
#[case::json_multi_event_accumulates_lines(      EncoderConfig::JsonLines { precision: None },                           "multi_json",   true)]
fn multi_event_pipeline_accumulates_correct_lines(
    #[case] config: EncoderConfig,
    #[case] metric_name: &str,
    #[case] validate_json: bool,
) {
    let encoder = create_encoder(&config).unwrap();
    let mut sink = MemorySink::new();
    for i in 0..5u64 {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + i * 1000);
        let labels = Labels::from_pairs(&[("idx", "0")]).unwrap();
        let event = MetricEvent::with_timestamp(metric_name.to_owned(), i as f64, labels, ts).unwrap();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();
        sink.write(&buf).unwrap();
    }
    sink.flush().unwrap();
    let text = std::str::from_utf8(&sink.buffer).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5);
    for line in &lines {
        if validate_json {
            let p: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(p["name"], metric_name);
        } else {
            assert!(line.starts_with(metric_name));
        }
    }
}

// ---------------------------------------------------------------------------
// YAML deserialization — encoder × sink combinations
// ---------------------------------------------------------------------------

use sonda_core::config::ScenarioConfig;
use sonda_core::encoder::EncoderConfig as EC;
use sonda_core::sink::SinkConfig as SC;

fn parse_scenario(yaml: &str) -> ScenarioConfig {
    serde_yaml_ng::from_str(yaml).unwrap_or_else(|e| panic!("YAML parse failed: {e}"))
}

fn base_yaml(enc: &str, sink: &str) -> String {
    format!("name: t\nrate: 1.0\ngenerator:\n  type: constant\n  value: 1.0\nencoder:\n{enc}\nsink:\n{sink}\n")
}

/// All 12 always-on encoder × sink YAML combinations round-trip through serde.
#[rstest]
#[rustfmt::skip]
#[case::prometheus_x_stdout("  type: prometheus_text", "  type: stdout")]
#[case::prometheus_x_file(  "  type: prometheus_text", "  type: file\n  path: /tmp/prom.txt")]
#[case::prometheus_x_tcp(   "  type: prometheus_text", "  type: tcp\n  address: \"127.0.0.1:9001\"")]
#[case::prometheus_x_udp(   "  type: prometheus_text", "  type: udp\n  address: \"127.0.0.1:9001\"")]
#[case::influx_x_stdout(    "  type: influx_lp",       "  type: stdout")]
#[case::influx_x_file(      "  type: influx_lp",       "  type: file\n  path: /tmp/influx.txt")]
#[case::influx_x_tcp(       "  type: influx_lp",       "  type: tcp\n  address: \"127.0.0.1:9002\"")]
#[case::influx_x_udp(       "  type: influx_lp",       "  type: udp\n  address: \"127.0.0.1:9002\"")]
#[case::json_x_stdout(      "  type: json_lines",      "  type: stdout")]
#[case::json_x_file(        "  type: json_lines",      "  type: file\n  path: /tmp/json.txt")]
#[case::json_x_tcp(         "  type: json_lines",      "  type: tcp\n  address: \"127.0.0.1:9003\"")]
#[case::json_x_udp(         "  type: json_lines",      "  type: udp\n  address: \"127.0.0.1:9003\"")]
fn yaml_encoder_sink_deserializes(#[case] enc: &str, #[case] sink: &str) {
    let s = parse_scenario(&base_yaml(enc, sink));
    if enc.contains("prometheus_text") { assert!(matches!(s.encoder, EC::PrometheusText { .. })); }
    else if enc.contains("influx_lp")  { assert!(matches!(s.encoder, EC::InfluxLineProtocol { .. })); }
    else                               { assert!(matches!(s.encoder, EC::JsonLines { .. })); }
    if sink.contains("stdout")      { assert!(matches!(s.sink, SC::Stdout)); }
    else if sink.contains("file")   { assert!(matches!(s.sink, SC::File { .. })); }
    else if sink.contains("tcp")    { assert!(matches!(s.sink, SC::Tcp { .. })); }
    else                            { assert!(matches!(s.sink, SC::Udp { .. })); }
}

#[cfg(feature = "http")]
#[rstest]
#[rustfmt::skip]
#[case::prometheus_x_http_push("  type: prometheus_text", "  type: http_push\n  url: \"http://localhost:9090/push\"")]
#[case::influx_x_http_push(    "  type: influx_lp",       "  type: http_push\n  url: \"http://localhost:8086/write\"")]
#[case::json_x_http_push(      "  type: json_lines",      "  type: http_push\n  url: \"http://localhost:9200/_bulk\"")]
fn yaml_encoder_http_push_deserializes(#[case] enc: &str, #[case] sink: &str) {
    let s = parse_scenario(&base_yaml(enc, sink));
    assert!(matches!(s.sink, SC::HttpPush { .. }));
    if enc.contains("prometheus_text") { assert!(matches!(s.encoder, EC::PrometheusText { .. })); }
    else if enc.contains("influx_lp")  { assert!(matches!(s.encoder, EC::InfluxLineProtocol { .. })); }
    else                               { assert!(matches!(s.encoder, EC::JsonLines { .. })); }
}

#[cfg(feature = "kafka")]
#[rstest]
#[rustfmt::skip]
#[case::prometheus_x_kafka("  type: prometheus_text", "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-prom")]
#[case::influx_x_kafka(    "  type: influx_lp",       "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-influx")]
#[case::json_x_kafka(      "  type: json_lines",      "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-json")]
fn yaml_encoder_kafka_deserializes(#[case] enc: &str, #[case] sink: &str) {
    let s = parse_scenario(&base_yaml(enc, sink));
    assert!(matches!(s.sink, SC::Kafka { .. }));
    if enc.contains("prometheus_text") { assert!(matches!(s.encoder, EC::PrometheusText { .. })); }
    else if enc.contains("influx_lp")  { assert!(matches!(s.encoder, EC::InfluxLineProtocol { .. })); }
    else                               { assert!(matches!(s.encoder, EC::JsonLines { .. })); }
}

// ---------------------------------------------------------------------------
// Encoder factory
// ---------------------------------------------------------------------------

/// Factory produces non-empty, newline-terminated output for every encoder.
#[rstest]
#[rustfmt::skip]
#[case::prometheus_text(       EncoderConfig::PrometheusText { precision: None })]
#[case::influx_lp_default_key( EncoderConfig::InfluxLineProtocol { field_key: None, precision: None })]
#[case::influx_lp_custom_key(  EncoderConfig::InfluxLineProtocol { field_key: Some("count".to_owned()), precision: None })]
#[case::json_lines(            EncoderConfig::JsonLines { precision: None })]
fn factory_encoder_produces_nonempty_output(#[case] config: EncoderConfig) {
    let encoder = create_encoder(&config).unwrap();
    let mut buf = Vec::new();
    encoder.encode_metric(&test_event(), &mut buf).unwrap();
    assert!(!buf.is_empty());
    assert_eq!(*buf.last().unwrap(), b'\n');
}

// ---------------------------------------------------------------------------
// Regression anchors
// ---------------------------------------------------------------------------

#[test]
fn regression_prometheus_exact_bytes() {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_owned(), 1.0, labels, ts).unwrap();
    let mut buf = Vec::new();
    create_encoder(&EncoderConfig::PrometheusText { precision: None })
        .unwrap()
        .encode_metric(&event, &mut buf)
        .unwrap();
    assert_eq!(buf, b"up{host=\"srv1\"} 1 1700000000000\n");
}

#[test]
fn regression_influx_exact_bytes() {
    let ts = UNIX_EPOCH + Duration::from_nanos(1_700_000_000_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_owned(), 1.0, labels, ts).unwrap();
    let mut buf = Vec::new();
    create_encoder(&EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    })
    .unwrap()
    .encode_metric(&event, &mut buf)
    .unwrap();
    assert_eq!(buf, b"up,host=srv1 value=1 1700000000000000000\n");
}

#[test]
fn regression_json_exact_bytes() {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_owned(), 1.0, labels, ts).unwrap();
    let mut buf = Vec::new();
    create_encoder(&EncoderConfig::JsonLines { precision: None })
        .unwrap()
        .encode_metric(&event, &mut buf)
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&buf).unwrap(),
        "{\"name\":\"up\",\"value\":1.0,\"labels\":{\"host\":\"srv1\"},\"timestamp\":\"2023-11-14T22:13:20.000Z\"}\n"
    );
}
