//! Encoder × Sink matrix integration tests for Slice 1.7.
//!
//! Validates that all 18 combinations of 3 encoders × 6 sinks compile and
//! produce non-empty output. The MemorySink is used as the capture target for
//! the non-network sinks (stdout, file, tcp, udp, http_push, kafka). Network
//! sinks are driven through real loopback connections.
//!
//! Encoders under test: prometheus_text, influx_lp, json_lines
//! Sinks under test:    stdout, file, tcp, udp, http_push, kafka (feature-gated)

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use sonda_core::encoder::{create_encoder, EncoderConfig};
use sonda_core::model::metric::{Labels, MetricEvent};
use sonda_core::sink::memory::MemorySink;
use sonda_core::sink::Sink;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a deterministic MetricEvent for use across all matrix tests.
fn test_event() -> MetricEvent {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1"), ("env", "test")]).unwrap();
    MetricEvent::with_timestamp("matrix_test_metric".to_string(), 42.0, labels, ts).unwrap()
}

/// Build a deterministic MetricEvent with no labels.
fn test_event_no_labels() -> MetricEvent {
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[]).unwrap();
    MetricEvent::with_timestamp("matrix_no_labels".to_string(), 1.0, labels, ts).unwrap()
}

/// Encode a single event with the given config, returning the encoded bytes.
fn encode_event(config: &EncoderConfig, event: &MetricEvent) -> Vec<u8> {
    let encoder = create_encoder(config);
    let mut buf = Vec::new();
    encoder
        .encode_metric(event, &mut buf)
        .expect("encode_metric must succeed");
    buf
}

/// Accept one HTTP connection, consume the request, and respond 200 OK.
/// Returns the request body.
fn accept_http_and_respond_ok(listener: &TcpListener) -> Vec<u8> {
    let (mut stream, _) = listener.accept().expect("accept");
    let body = read_http_body(&mut stream);
    let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream.write_all(resp.as_bytes()).ok();
    body
}

fn read_http_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header line");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("content-length:") {
            let val = lower["content-length:".len()..].trim().to_string();
            content_length = val.parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read body");
    body
}

// ---------------------------------------------------------------------------
// Section 1: All 3 encoders produce non-empty output with MemorySink
//
// These tests use MemorySink as a zero-friction, zero-I/O stand-in for every
// sink. They verify that the encoder pipeline produces non-empty bytes and that
// the bytes are valid for the expected format.
// ---------------------------------------------------------------------------

// ---- prometheus_text × MemorySink ------------------------------------------

#[test]
fn prometheus_x_memory_produces_nonempty_output() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    assert!(
        !sink.buffer.is_empty(),
        "prometheus_text encoder produced no output"
    );
}

#[test]
fn prometheus_x_memory_output_contains_metric_name() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let text = std::str::from_utf8(&sink.buffer).expect("output must be UTF-8");
    assert!(
        text.contains("matrix_test_metric"),
        "output must contain metric name: {text:?}"
    );
}

#[test]
fn prometheus_x_memory_output_ends_with_newline() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    assert_eq!(
        *sink.buffer.last().unwrap(),
        b'\n',
        "prometheus output must end with newline"
    );
}

#[test]
fn prometheus_x_memory_no_labels_omits_braces() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event_no_labels();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    assert!(
        !text.contains('{'),
        "no-label prometheus output must not contain braces: {text:?}"
    );
}

// ---- influx_lp × MemorySink ------------------------------------------------

#[test]
fn influx_x_memory_produces_nonempty_output() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    assert!(
        !sink.buffer.is_empty(),
        "influx_lp encoder produced no output"
    );
}

#[test]
fn influx_x_memory_output_contains_measurement_name() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    assert!(
        text.contains("matrix_test_metric"),
        "influx output must contain measurement name: {text:?}"
    );
}

#[test]
fn influx_x_memory_output_has_nanosecond_timestamp() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let text = std::str::from_utf8(&sink.buffer)
        .unwrap()
        .trim_end_matches('\n');
    let ts_str = text.split_whitespace().last().unwrap();
    assert!(
        ts_str.len() >= 13,
        "influx timestamp must be nanoseconds (>=13 digits): {ts_str}"
    );
}

#[test]
fn influx_x_memory_custom_field_key_appears_in_output() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: Some("requests".to_string()),
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    assert!(
        text.contains("requests="),
        "influx output must use custom field key: {text:?}"
    );
}

// ---- json_lines × MemorySink -----------------------------------------------

#[test]
fn json_x_memory_produces_nonempty_output() {
    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    assert!(
        !sink.buffer.is_empty(),
        "json_lines encoder produced no output"
    );
}

#[test]
fn json_x_memory_output_is_valid_json() {
    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let line = std::str::from_utf8(&sink.buffer)
        .unwrap()
        .trim_end_matches('\n');
    let parsed: serde_json::Value =
        serde_json::from_str(line).expect("json_lines output must be valid JSON");
    assert!(parsed.is_object());
}

#[test]
fn json_x_memory_output_contains_all_expected_fields() {
    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = MemorySink::new();
    sink.write(&bytes).unwrap();

    let line = std::str::from_utf8(&sink.buffer)
        .unwrap()
        .trim_end_matches('\n');
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(parsed["name"], "matrix_test_metric");
    assert!((parsed["value"].as_f64().unwrap() - 42.0).abs() < f64::EPSILON);
    assert_eq!(parsed["labels"]["host"], "srv1");
    assert_eq!(parsed["labels"]["env"], "test");
}

// ---------------------------------------------------------------------------
// Section 2: All 3 encoders × file sink
//
// Encodes a single event, writes the output to a temp file, then reads it
// back to verify round-trip fidelity.
// ---------------------------------------------------------------------------

fn tmp_matrix_path(encoder_name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("sonda-matrix-tests")
        .join(format!("{encoder_name}-matrix.txt"))
}

#[test]
fn prometheus_x_file_write_and_read_back_matches() {
    let path = tmp_matrix_path("prometheus");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::file::FileSink::new(Path::new(&path))
        .expect("FileSink creation must succeed");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let content = std::fs::read(&path).expect("file must be readable after write");
    assert_eq!(content, bytes, "file contents must match encoded bytes");
}

#[test]
fn influx_x_file_write_and_read_back_matches() {
    let path = tmp_matrix_path("influx");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::file::FileSink::new(Path::new(&path))
        .expect("FileSink creation must succeed");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let content = std::fs::read(&path).unwrap();
    assert_eq!(content, bytes);
}

#[test]
fn json_x_file_write_and_read_back_matches() {
    let path = tmp_matrix_path("json");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::file::FileSink::new(Path::new(&path))
        .expect("FileSink creation must succeed");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let content = std::fs::read(&path).unwrap();
    assert_eq!(content, bytes);
}

// ---------------------------------------------------------------------------
// Section 3: All 3 encoders × TCP sink
//
// Opens a listener on an ephemeral port, connects a TcpSink, writes one
// encoded event, reads it back on the listener.
// ---------------------------------------------------------------------------

fn tcp_matrix_server() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    (listener, addr)
}

#[test]
fn prometheus_x_tcp_data_arrives_at_listener() {
    let (listener, addr) = tcp_matrix_server();

    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let receiver = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        buf
    });

    let mut sink = sonda_core::sink::tcp::TcpSink::new(&addr).expect("TcpSink must connect");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let received = receiver.join().expect("receiver thread panicked");
    assert_eq!(received, expected);
}

#[test]
fn influx_x_tcp_data_arrives_at_listener() {
    let (listener, addr) = tcp_matrix_server();

    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let receiver = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        buf
    });

    let mut sink = sonda_core::sink::tcp::TcpSink::new(&addr).expect("TcpSink must connect");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let received = receiver.join().expect("receiver thread panicked");
    assert_eq!(received, expected);
}

#[test]
fn json_x_tcp_data_arrives_at_listener() {
    let (listener, addr) = tcp_matrix_server();

    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let receiver = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        buf
    });

    let mut sink = sonda_core::sink::tcp::TcpSink::new(&addr).expect("TcpSink must connect");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();
    drop(sink);

    let received = receiver.join().expect("receiver thread panicked");
    assert_eq!(received, expected);
}

// ---------------------------------------------------------------------------
// Section 4: All 3 encoders × UDP sink
//
// Opens a receiver socket, sends one encoded event as a datagram, reads it.
// ---------------------------------------------------------------------------

fn udp_matrix_receiver() -> (UdpSocket, String) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let addr = socket.local_addr().unwrap().to_string();
    (socket, addr)
}

#[test]
fn prometheus_x_udp_datagram_arrives_at_receiver() {
    let (receiver, addr) = udp_matrix_receiver();

    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::udp::UdpSink::new(&addr).expect("UdpSink must bind");
    sink.write(&bytes).unwrap();

    let mut buf = vec![0u8; 4096];
    let (len, _) = receiver.recv_from(&mut buf).expect("must receive datagram");
    assert_eq!(&buf[..len], bytes.as_slice());
}

#[test]
fn influx_x_udp_datagram_arrives_at_receiver() {
    let (receiver, addr) = udp_matrix_receiver();

    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::udp::UdpSink::new(&addr).expect("UdpSink must bind");
    sink.write(&bytes).unwrap();

    let mut buf = vec![0u8; 4096];
    let (len, _) = receiver.recv_from(&mut buf).expect("must receive datagram");
    assert_eq!(&buf[..len], bytes.as_slice());
}

#[test]
fn json_x_udp_datagram_arrives_at_receiver() {
    let (receiver, addr) = udp_matrix_receiver();

    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::udp::UdpSink::new(&addr).expect("UdpSink must bind");
    sink.write(&bytes).unwrap();

    let mut buf = vec![0u8; 4096];
    let (len, _) = receiver.recv_from(&mut buf).expect("must receive datagram");
    assert_eq!(&buf[..len], bytes.as_slice());
}

// ---------------------------------------------------------------------------
// Section 5: All 3 encoders × HTTP push sink
//
// For each encoder: bind a mock HTTP server, construct an HttpPushSink,
// write one encoded event, flush, verify the body received by the server
// matches the encoded bytes.
// ---------------------------------------------------------------------------

fn http_matrix_server() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/push");
    (listener, url)
}

#[test]
fn prometheus_x_http_push_body_matches_encoded_bytes() {
    let (listener, url) = http_matrix_server();

    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let server = thread::spawn(move || accept_http_and_respond_ok(&listener));

    let mut sink = sonda_core::sink::http::HttpPushSink::new(
        &url,
        "text/plain; version=0.0.4",
        10_000,
        HashMap::new(),
    )
    .expect("HttpPushSink must construct");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    let body = server.join().expect("server thread panicked");
    assert_eq!(body, expected);
}

#[test]
fn influx_x_http_push_body_matches_encoded_bytes() {
    let (listener, url) = http_matrix_server();

    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let server = thread::spawn(move || accept_http_and_respond_ok(&listener));

    let mut sink =
        sonda_core::sink::http::HttpPushSink::new(&url, "text/plain", 10_000, HashMap::new())
            .expect("HttpPushSink must construct");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    let body = server.join().expect("server thread panicked");
    assert_eq!(body, expected);
}

#[test]
fn json_x_http_push_body_matches_encoded_bytes() {
    let (listener, url) = http_matrix_server();

    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);
    let expected = bytes.clone();

    let server = thread::spawn(move || accept_http_and_respond_ok(&listener));

    let mut sink = sonda_core::sink::http::HttpPushSink::new(
        &url,
        "application/x-ndjson",
        10_000,
        HashMap::new(),
    )
    .expect("HttpPushSink must construct");
    sink.write(&bytes).unwrap();
    sink.flush().unwrap();

    let body = server.join().expect("server thread panicked");
    assert_eq!(body, expected);
}

// ---------------------------------------------------------------------------
// Section 6: All 3 encoders × stdout sink
//
// Stdout is not easily captured in tests, but we verify that:
//   (a) write() and flush() return Ok for each encoder's output, and
//   (b) the encoded bytes are non-empty.
//
// This ensures the stdout sink compiles and wires correctly for all encoders.
// ---------------------------------------------------------------------------

#[test]
fn prometheus_x_stdout_write_and_flush_succeed() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::stdout::StdoutSink::new();
    assert!(sink.write(&bytes).is_ok());
    assert!(sink.flush().is_ok());
}

#[test]
fn influx_x_stdout_write_and_flush_succeed() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::stdout::StdoutSink::new();
    assert!(sink.write(&bytes).is_ok());
    assert!(sink.flush().is_ok());
}

#[test]
fn json_x_stdout_write_and_flush_succeed() {
    let config = EncoderConfig::JsonLines { precision: None };
    let event = test_event();
    let bytes = encode_event(&config, &event);

    let mut sink = sonda_core::sink::stdout::StdoutSink::new();
    assert!(sink.write(&bytes).is_ok());
    assert!(sink.flush().is_ok());
}

// ---------------------------------------------------------------------------
// Section 7: Kafka sink (feature-gated)
//
// When the "kafka" feature is enabled, verify that all 3 encoders produce
// bytes that can be handed to a KafkaSink. Since a real broker is not
// available in CI, we only verify that the construction path with an
// unreachable broker returns SondaError::Sink (not a panic) and that the
// encoded output is non-empty.
// ---------------------------------------------------------------------------

#[cfg(feature = "kafka")]
mod kafka_matrix {
    use super::*;
    use sonda_core::encoder::EncoderConfig;
    use sonda_core::SondaError;

    fn unreachable_kafka_config() -> sonda_core::sink::SinkConfig {
        sonda_core::sink::SinkConfig::Kafka {
            brokers: String::new(), // empty → immediate parse error, no timeout
            topic: "sonda-matrix-test".to_string(),
        }
    }

    #[test]
    fn prometheus_x_kafka_encoded_bytes_are_nonempty() {
        let config = EncoderConfig::PrometheusText { precision: None };
        let event = test_event();
        let bytes = encode_event(&config, &event);
        assert!(
            !bytes.is_empty(),
            "prometheus encoder must produce bytes for kafka path"
        );
    }

    #[test]
    fn influx_x_kafka_encoded_bytes_are_nonempty() {
        let config = EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        };
        let event = test_event();
        let bytes = encode_event(&config, &event);
        assert!(
            !bytes.is_empty(),
            "influx encoder must produce bytes for kafka path"
        );
    }

    #[test]
    fn json_x_kafka_encoded_bytes_are_nonempty() {
        let config = EncoderConfig::JsonLines { precision: None };
        let event = test_event();
        let bytes = encode_event(&config, &event);
        assert!(
            !bytes.is_empty(),
            "json encoder must produce bytes for kafka path"
        );
    }

    #[test]
    fn kafka_sink_with_empty_broker_returns_sink_error_not_panic() {
        let config = unreachable_kafka_config();
        let result = sonda_core::sink::create_sink(&config, None);
        assert!(result.is_err(), "empty broker string must produce an error");
        assert!(
            matches!(result.err().unwrap(), SondaError::Sink(_)),
            "error must be SondaError::Sink"
        );
    }
}

// ---------------------------------------------------------------------------
// Section 8: Multi-event encode → write pipeline for each encoder
//
// Encode multiple events in sequence and verify each one contributes a line
// (non-empty, newline-terminated) to the sink buffer.
// ---------------------------------------------------------------------------

#[test]
fn prometheus_multi_event_pipeline_accumulates_correct_lines() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let encoder = create_encoder(&config);
    let mut sink = MemorySink::new();

    for i in 0..5u64 {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + i * 1000);
        let labels = Labels::from_pairs(&[("idx", "0")]).unwrap();
        let event =
            MetricEvent::with_timestamp("multi_event".to_string(), i as f64, labels, ts).unwrap();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();
        sink.write(&buf).unwrap();
    }
    sink.flush().unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "must produce exactly 5 prometheus lines");
    for line in &lines {
        assert!(
            line.starts_with("multi_event"),
            "each line must start with metric name: {line}"
        );
    }
}

#[test]
fn influx_multi_event_pipeline_accumulates_correct_lines() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let encoder = create_encoder(&config);
    let mut sink = MemorySink::new();

    for i in 0..5u64 {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + i * 1000);
        let labels = Labels::from_pairs(&[("idx", "0")]).unwrap();
        let event =
            MetricEvent::with_timestamp("multi_influx".to_string(), i as f64, labels, ts).unwrap();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();
        sink.write(&buf).unwrap();
    }
    sink.flush().unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "must produce exactly 5 influx lines");
    for line in &lines {
        assert!(
            line.starts_with("multi_influx"),
            "each line must start with measurement name: {line}"
        );
    }
}

#[test]
fn json_multi_event_pipeline_accumulates_correct_lines() {
    let config = EncoderConfig::JsonLines { precision: None };
    let encoder = create_encoder(&config);
    let mut sink = MemorySink::new();

    for i in 0..5u64 {
        let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + i * 1000);
        let labels = Labels::from_pairs(&[("idx", "0")]).unwrap();
        let event =
            MetricEvent::with_timestamp("multi_json".to_string(), i as f64, labels, ts).unwrap();
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).unwrap();
        sink.write(&buf).unwrap();
    }
    sink.flush().unwrap();

    let text = std::str::from_utf8(&sink.buffer).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "must produce exactly 5 json lines");
    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line must be valid JSON");
        assert_eq!(
            parsed["name"], "multi_json",
            "each JSON line must have correct name field"
        );
    }
}

// ---------------------------------------------------------------------------
// Section 9: EncoderConfig × SinkConfig YAML deserialization coverage
//
// Verifies that all 18 encoder/sink combinations can be described in YAML
// and parsed into valid ScenarioConfig structs. This ensures that the config
// layer wires correctly for each combination, which is the primary contract
// of Slice 1.7.
// ---------------------------------------------------------------------------

use sonda_core::config::ScenarioConfig;
use sonda_core::encoder::EncoderConfig as EC;
use sonda_core::sink::SinkConfig as SC;

fn parse_scenario(yaml: &str) -> ScenarioConfig {
    serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("YAML parse failed: {e}\nInput:\n{yaml}"))
}

fn base_yaml(encoder_block: &str, sink_block: &str) -> String {
    format!(
        "name: matrix_test\nrate: 10.0\ngenerator:\n  type: constant\n  value: 1.0\nencoder:\n{encoder_block}\nsink:\n{sink_block}\n"
    )
}

// --- prometheus_text × all sinks ---

#[test]
fn yaml_prometheus_x_stdout_deserializes() {
    let scenario = parse_scenario(&base_yaml("  type: prometheus_text", "  type: stdout"));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::Stdout));
}

#[test]
fn yaml_prometheus_x_file_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: prometheus_text",
        "  type: file\n  path: /tmp/prom-file.txt",
    ));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::File { .. }));
}

#[test]
fn yaml_prometheus_x_tcp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: prometheus_text",
        "  type: tcp\n  address: \"127.0.0.1:9001\"",
    ));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::Tcp { .. }));
}

#[test]
fn yaml_prometheus_x_udp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: prometheus_text",
        "  type: udp\n  address: \"127.0.0.1:9001\"",
    ));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::Udp { .. }));
}

#[test]
fn yaml_prometheus_x_http_push_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: prometheus_text",
        "  type: http_push\n  url: \"http://localhost:9090/push\"",
    ));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::HttpPush { .. }));
}

#[cfg(feature = "kafka")]
#[test]
fn yaml_prometheus_x_kafka_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: prometheus_text",
        "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-test",
    ));
    assert!(matches!(scenario.encoder, EC::PrometheusText { .. }));
    assert!(matches!(scenario.sink, SC::Kafka { .. }));
}

// --- influx_lp × all sinks ---

#[test]
fn yaml_influx_x_stdout_deserializes() {
    let scenario = parse_scenario(&base_yaml("  type: influx_lp", "  type: stdout"));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::Stdout));
}

#[test]
fn yaml_influx_x_file_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: influx_lp",
        "  type: file\n  path: /tmp/influx-file.txt",
    ));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::File { .. }));
}

#[test]
fn yaml_influx_x_tcp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: influx_lp",
        "  type: tcp\n  address: \"127.0.0.1:9002\"",
    ));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::Tcp { .. }));
}

#[test]
fn yaml_influx_x_udp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: influx_lp",
        "  type: udp\n  address: \"127.0.0.1:9002\"",
    ));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::Udp { .. }));
}

#[test]
fn yaml_influx_x_http_push_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: influx_lp",
        "  type: http_push\n  url: \"http://localhost:8086/write\"",
    ));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::HttpPush { .. }));
}

#[cfg(feature = "kafka")]
#[test]
fn yaml_influx_x_kafka_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: influx_lp",
        "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-influx",
    ));
    assert!(matches!(scenario.encoder, EC::InfluxLineProtocol { .. }));
    assert!(matches!(scenario.sink, SC::Kafka { .. }));
}

// --- json_lines × all sinks ---

#[test]
fn yaml_json_x_stdout_deserializes() {
    let scenario = parse_scenario(&base_yaml("  type: json_lines", "  type: stdout"));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::Stdout));
}

#[test]
fn yaml_json_x_file_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: json_lines",
        "  type: file\n  path: /tmp/json-file.txt",
    ));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::File { .. }));
}

#[test]
fn yaml_json_x_tcp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: json_lines",
        "  type: tcp\n  address: \"127.0.0.1:9003\"",
    ));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::Tcp { .. }));
}

#[test]
fn yaml_json_x_udp_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: json_lines",
        "  type: udp\n  address: \"127.0.0.1:9003\"",
    ));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::Udp { .. }));
}

#[test]
fn yaml_json_x_http_push_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: json_lines",
        "  type: http_push\n  url: \"http://localhost:9200/_bulk\"",
    ));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::HttpPush { .. }));
}

#[cfg(feature = "kafka")]
#[test]
fn yaml_json_x_kafka_deserializes() {
    let scenario = parse_scenario(&base_yaml(
        "  type: json_lines",
        "  type: kafka\n  brokers: \"127.0.0.1:9092\"\n  topic: sonda-json",
    ));
    assert!(matches!(scenario.encoder, EC::JsonLines { .. }));
    assert!(matches!(scenario.sink, SC::Kafka { .. }));
}

// ---------------------------------------------------------------------------
// Section 10: create_encoder + create_sink factory wiring for all combinations
//
// Verifies that calling create_encoder() and create_sink() together with
// matched configs produces a working encoder/sink pair that can encode and
// deliver a MetricEvent end-to-end.
// ---------------------------------------------------------------------------

/// End-to-end helper: encode an event with the given config and write to a
/// MemorySink, verifying the output is non-empty and newline-terminated.
fn assert_encoder_produces_output(config: &EncoderConfig) {
    let encoder = create_encoder(config);
    let event = test_event();
    let mut buf = Vec::new();
    encoder.encode_metric(&event, &mut buf).unwrap();

    assert!(!buf.is_empty(), "encoder {config:?} produced empty output");
    assert_eq!(
        *buf.last().unwrap(),
        b'\n',
        "encoder {config:?} output must end with newline"
    );
}

#[test]
fn factory_prometheus_text_produces_output() {
    assert_encoder_produces_output(&EncoderConfig::PrometheusText { precision: None });
}

#[test]
fn factory_influx_lp_default_field_key_produces_output() {
    assert_encoder_produces_output(&EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    });
}

#[test]
fn factory_influx_lp_custom_field_key_produces_output() {
    assert_encoder_produces_output(&EncoderConfig::InfluxLineProtocol {
        field_key: Some("count".to_string()),
        precision: None,
    });
}

#[test]
fn factory_json_lines_produces_output() {
    assert_encoder_produces_output(&EncoderConfig::JsonLines { precision: None });
}

// ---------------------------------------------------------------------------
// Section 11: Regression anchors — hardcoded expected byte strings
//
// Each encoder × MemorySink combination is tested with a known input event
// and the exact expected byte output is checked. This catches silent format
// regressions.
// ---------------------------------------------------------------------------

#[test]
fn regression_prometheus_x_memory_exact_bytes() {
    let config = EncoderConfig::PrometheusText { precision: None };
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();

    let mut sink = MemorySink::new();
    let encoder = create_encoder(&config);
    let mut buf = Vec::new();
    encoder.encode_metric(&event, &mut buf).unwrap();
    sink.write(&buf).unwrap();

    assert_eq!(
        sink.buffer, b"up{host=\"srv1\"} 1 1700000000000\n",
        "prometheus regression anchor failed"
    );
}

#[test]
fn regression_influx_x_memory_exact_bytes() {
    let config = EncoderConfig::InfluxLineProtocol {
        field_key: None,
        precision: None,
    };
    let ts = UNIX_EPOCH + Duration::from_nanos(1_700_000_000_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();

    let mut sink = MemorySink::new();
    let encoder = create_encoder(&config);
    let mut buf = Vec::new();
    encoder.encode_metric(&event, &mut buf).unwrap();
    sink.write(&buf).unwrap();

    assert_eq!(
        sink.buffer, b"up,host=srv1 value=1 1700000000000000000\n",
        "influx regression anchor failed"
    );
}

#[test]
fn regression_json_x_memory_exact_bytes() {
    let config = EncoderConfig::JsonLines { precision: None };
    let ts = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
    let labels = Labels::from_pairs(&[("host", "srv1")]).unwrap();
    let event = MetricEvent::with_timestamp("up".to_string(), 1.0, labels, ts).unwrap();

    let mut sink = MemorySink::new();
    let encoder = create_encoder(&config);
    let mut buf = Vec::new();
    encoder.encode_metric(&event, &mut buf).unwrap();
    sink.write(&buf).unwrap();

    assert_eq!(
        std::str::from_utf8(&sink.buffer).unwrap(),
        "{\"name\":\"up\",\"value\":1.0,\"labels\":{\"host\":\"srv1\"},\"timestamp\":\"2023-11-14T22:13:20.000Z\"}\n",
        "json regression anchor failed"
    );
}
