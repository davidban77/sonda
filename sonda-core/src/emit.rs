//! Single-event emission helpers.

use std::collections::HashMap;

use crate::encoder::{create_encoder, EncoderConfig};
use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::sink::{create_sink, SinkConfig};
use crate::SondaError;

/// Encode a [`LogEvent`] and deliver it through a one-shot sink.
pub async fn emit_log(
    event: &LogEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    let encoder = create_encoder(encoder)?;
    let mut sink = create_sink(sink, labels).await?;
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_log(event, &mut buf)?;
    sink.write_log_event(event, &buf).await?;
    sink.flush().await
}

/// Encode a [`MetricEvent`] and deliver it through a one-shot sink.
pub async fn emit_metric(
    event: &MetricEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    let encoder = create_encoder(encoder)?;
    let mut sink = create_sink(sink, labels).await?;
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_metric(event, &mut buf)?;
    sink.write(&buf).await?;
    sink.flush().await
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

    #[tokio::test]
    async fn emit_log_writes_encoded_line_to_sink() {
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
        .await
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

    #[tokio::test]
    async fn emit_metric_writes_encoded_line_to_sink() {
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
        .await
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

    #[tokio::test]
    async fn emit_log_propagates_config_error_for_invalid_sink_config() {
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
        .await
        .expect_err("invalid retry config must fail sink construction");

        assert!(
            matches!(err, SondaError::Config(_)),
            "invalid retry config must surface as SondaError::Config, got: {err:?}"
        );
    }

    #[cfg(feature = "http")]
    #[tokio::test(flavor = "multi_thread")]
    async fn emit_log_routes_event_labels_to_loki_sink_stream() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::thread;

        fn read_http_body(stream: &mut TcpStream) -> Vec<u8> {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header line");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(rest) = lower.strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).expect("body");
            body
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let url = format!("http://127.0.0.1:{port}");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let body = read_http_body(&mut stream);
            let resp = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).ok();
            body
        });

        let labels = Labels::from_pairs(&[("peer_address", "10.1.2.2")]).expect("labels");
        let event = LogEvent::new(
            Severity::Info,
            "bgp event".to_string(),
            labels,
            BTreeMap::new(),
        );

        emit_log(
            &event,
            &EncoderConfig::JsonLines { precision: None },
            &SinkConfig::Loki {
                url,
                batch_size: Some(1),
                max_streams_per_push: None,
                max_buffer_age: None,
                retry: None,
            },
            None,
        )
        .await
        .expect("emit_log must succeed");

        let body_bytes = handle.join().expect("mock server");
        let body = String::from_utf8(body_bytes).expect("UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON envelope");

        let stream_obj = parsed["streams"][0]["stream"]
            .as_object()
            .expect("stream object");
        assert_eq!(
            stream_obj.get("peer_address").and_then(|v| v.as_str()),
            Some("10.1.2.2"),
            "event labels must reach the Loki stream via write_log_event: {body}"
        );
    }

    #[tokio::test]
    async fn emit_metric_propagates_config_error_for_invalid_sink_config() {
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
        .await
        .expect_err("invalid retry config must fail sink construction");

        assert!(
            matches!(err, SondaError::Config(_)),
            "invalid retry config must surface as SondaError::Config, got: {err:?}"
        );
    }
}
