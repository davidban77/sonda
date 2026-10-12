//! Single-event emission helpers.

use std::collections::HashMap;

use crate::config::validate::{reject_gnmi_for_logs, validate_encoder_sink_pairing};
use crate::encoder::{create_encoder, EncoderConfig};
use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::sink::{create_sink, SinkConfig};
use crate::SondaError;

/// Encode a [`LogEvent`] and deliver it through a one-shot sink.
///
/// Returns a [`SondaError::Config`] before building anything when the encoder
/// cannot be paired with the sink (see [`validate_encoder_sink_pairing`]), or
/// when either is the `gnmi` encoder or the `gnmi_target` sink, which carry
/// metric signals only.
pub async fn emit_log(
    event: &LogEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    reject_gnmi_for_logs(encoder, sink)?;
    validate_encoder_sink_pairing(encoder, sink)?;
    let encoder = create_encoder(encoder)?;
    let mut sink = create_sink(sink, labels).await?;
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_log(event, &mut buf)?;
    sink.write_log_event(event, &buf).await?;
    sink.flush().await
}

/// Encode a [`MetricEvent`] and deliver it through a one-shot sink.
///
/// Returns a [`SondaError::Config`] before building anything when the encoder
/// cannot be paired with the sink (see [`validate_encoder_sink_pairing`]).
///
/// A `gnmi_target` sink joins the live target on its address and never
/// binds one: with no running scenario listening there, this is a
/// [`SondaError::Config`]. The event stays in the target's table after the
/// call returns.
pub async fn emit_metric(
    event: &MetricEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    validate_encoder_sink_pairing(encoder, sink)?;
    let encoder = create_encoder(encoder)?;
    let mut sink: Box<dyn crate::sink::Sink> = match sink {
        #[cfg(feature = "gnmi")]
        SinkConfig::GnmiTarget(cfg) => Box::new(crate::sink::gnmi::GnmiTargetSink::join(cfg)?),
        _ => create_sink(sink, labels).await?,
    };
    let mut buf: Vec<u8> = Vec::new();
    encoder.encode_metric(event, &mut buf)?;
    // Same rule as the schedule runners: only a sink that asked for the event
    // receives it; every other sink keeps the plain write path.
    if sink.wants_metric_events() {
        sink.write_metric_event(event, &buf).await?;
    } else {
        sink.write(&buf).await?;
    }
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

    /// The pairing rule applies to one-shot emission too: a gnmi encoder is
    /// refused before any sink is built, and nothing is written.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_rejects_the_gnmi_encoder_with_any_sink() {
        let path = temp_path("emit_gnmi_pairing");
        let _ = std::fs::remove_file(&path);
        let sink = SinkConfig::File {
            path: path.to_string_lossy().into_owned(),
        };
        let gnmi = EncoderConfig::Gnmi(crate::encoder::gnmi::GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            ..Default::default()
        });
        let metric =
            MetricEvent::new("in_octets".to_string(), 1.0, Labels::default()).expect("metric");
        let log = LogEvent::new(
            Severity::Info,
            "pairing probe".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );

        // Positive control: the same event and sink with another encoder is
        // delivered, so the rejection below comes from the encoder alone.
        emit_metric(
            &metric,
            &EncoderConfig::PrometheusText { precision: None },
            &sink,
            None,
        )
        .await
        .expect("prometheus_text to a file must succeed");
        assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
        let _ = std::fs::remove_file(&path);

        let metric_err = emit_metric(&metric, &gnmi, &sink, None)
            .await
            .expect_err("gnmi must be refused");
        let log_err = emit_log(&log, &gnmi, &sink, None)
            .await
            .expect_err("gnmi must be refused");
        for (err, needle) in [(metric_err, "gnmi_target"), (log_err, "not logs entries")] {
            assert!(matches!(err, SondaError::Config(_)), "got: {err:?}");
            assert!(err.to_string().contains(needle), "got: {err}");
        }
        assert!(
            std::fs::metadata(&path).is_err(),
            "no sink may be built for a refused pairing"
        );
    }

    /// A target held alive here stands in for a running scenario:
    /// `emit_metric` joins it, and the event stays in its table after the
    /// one-shot sink drops.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_metric_reaches_a_gnmi_target_through_the_hook() {
        use crate::encoder::gnmi::{proto, GnmiEncoderConfig};
        use crate::sink::gnmi::{GnmiTargetConfig, GnmiTargetSink};
        use prost::Message;

        let held = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let target = std::sync::Arc::clone(held.target());
        assert_eq!(target.series_count(), 0);

        let event = MetricEvent::new(
            "in_octets".to_string(),
            42.5,
            Labels::from_pairs(&[("ifName", "Gi0")]).expect("labels"),
        )
        .expect("metric event");
        let encoder = EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some("/interfaces/interface[name={ifName}]/{name}".to_string()),
            ..Default::default()
        });
        let sink = SinkConfig::GnmiTarget(GnmiTargetConfig::new(target.local_addr().to_string()));
        emit_metric(&event, &encoder, &sink, None)
            .await
            .expect("emit_metric must deliver to the target");

        assert_eq!(target.series_count(), 1, "the event is one series");
        let snapshot = target.snapshot();
        assert_eq!(snapshot.len(), 1);
        let notification =
            proto::Notification::decode(snapshot[0].latest.as_ref()).expect("a notification");
        assert_eq!(notification.update.len(), 1);
        let value = notification.update[0]
            .val
            .as_ref()
            .and_then(|v| v.value.as_ref());
        assert_eq!(
            value,
            Some(&proto::typed_value::Value::DoubleVal(42.5)),
            "the latest value is the event's"
        );
    }

    /// The gnmi encoder and the gnmi_target sink carry metrics only; a log
    /// sent to them is a config error, not an encode failure.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_log_rejects_the_gnmi_target_sink() {
        use crate::encoder::gnmi::GnmiEncoderConfig;
        use crate::sink::gnmi::GnmiTargetConfig;

        let log = LogEvent::new(
            Severity::Info,
            "probe".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        let gnmi = EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            ..Default::default()
        });
        let sink = SinkConfig::GnmiTarget(GnmiTargetConfig::new("127.0.0.1:0"));

        let err = emit_log(&log, &gnmi, &sink, None)
            .await
            .expect_err("a log on gnmi_target must be refused");
        assert!(matches!(err, SondaError::Config(_)), "got: {err:?}");
        assert!(err.to_string().contains("not logs entries"), "got: {err}");
    }

    /// With no running scenario on the address, `emit_metric` refuses the
    /// event instead of binding a target of its own.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_metric_without_a_live_gnmi_target_is_a_config_error() {
        use crate::encoder::gnmi::GnmiEncoderConfig;
        use crate::sink::gnmi::{GnmiTargetConfig, GnmiTargetSink};

        // A port a target held and released, so nothing listens on it now.
        let addr = {
            let held = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
                .await
                .expect("bind an ephemeral port");
            let addr = held.target().local_addr();
            let stopped = held.target().wait();
            drop(held);
            stopped.await;
            addr
        };
        let event = MetricEvent::new("in_octets".to_string(), 1.0, Labels::default())
            .expect("metric event");
        let encoder = EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            ..Default::default()
        });
        let sink = SinkConfig::GnmiTarget(GnmiTargetConfig::new(addr.to_string()));

        let err = emit_metric(&event, &encoder, &sink, None)
            .await
            .expect_err("nothing to join");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(
            err.to_string()
                .contains(&format!("no running scenario listens on {addr}")),
            "got {err}"
        );
        tokio::net::TcpStream::connect(addr)
            .await
            .expect_err("emit_metric bound nothing on the address");
    }
}
