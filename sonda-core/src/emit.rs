//! Single-event emission helpers.

use std::collections::HashMap;

use crate::config::validate::{
    reject_gnmi_for_logs, reject_snmp_for, validate_encoder_sink_pairing,
};
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
/// metric signals only, or the `snmp` encoder or the `snmp_agent` sink, which
/// carry metrics only.
pub async fn emit_log(
    event: &LogEvent,
    encoder: &EncoderConfig,
    sink: &SinkConfig,
    labels: Option<&HashMap<String, String>>,
) -> Result<(), SondaError> {
    reject_gnmi_for_logs(encoder, sink)?;
    reject_snmp_for("logs", encoder, sink)?;
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
/// call returns. An `snmp_agent` sink joins the live agent on its address in
/// the same way, and replaces the value of an instance the agent already
/// serves: an event for any other OID is a [`SondaError::Config`].
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
        #[cfg(feature = "snmp")]
        SinkConfig::SnmpAgent(cfg) => Box::new(crate::sink::snmp::SnmpAgentSink::join(cfg)?),
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

    /// A sink holding a target and publishing a series stands in for a
    /// running scenario.
    #[cfg(feature = "gnmi")]
    async fn running_scenario(
        encoder: &EncoderConfig,
        if_name: &str,
    ) -> crate::sink::gnmi::GnmiTargetSink {
        use crate::encoder::create_encoder;
        use crate::sink::gnmi::{GnmiTargetConfig, GnmiTargetSink};
        use crate::sink::Sink;

        let mut held = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let event = MetricEvent::new(
            "in_octets".to_string(),
            1.0,
            Labels::from_pairs(&[("ifName", if_name)]).expect("labels"),
        )
        .expect("metric event");
        let mut buf = Vec::new();
        create_encoder(encoder)
            .expect("valid encoder")
            .encode_metric(&event, &mut buf)
            .expect("encodes");
        held.write_metric_event(&event, &buf)
            .await
            .expect("the scenario publishes its series");
        held
    }

    #[cfg(feature = "gnmi")]
    fn interface_encoder() -> EncoderConfig {
        EncoderConfig::Gnmi(crate::encoder::gnmi::GnmiEncoderConfig {
            path: Some("/interfaces/interface[name={ifName}]/{name}".to_string()),
            ..Default::default()
        })
    }

    /// `emit_metric` joins the running target and replaces the latest value
    /// of a series it serves, through the metric hook, without owning it.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_metric_reaches_a_gnmi_target_through_the_hook() {
        use crate::encoder::gnmi::proto;
        use crate::sink::gnmi::GnmiTargetConfig;
        use prost::Message;

        let encoder = interface_encoder();
        let held = running_scenario(&encoder, "Gi0").await;
        let target = std::sync::Arc::clone(held.target());
        assert_eq!(target.series_count(), 1, "the scenario serves one series");

        let event = MetricEvent::new(
            "in_octets".to_string(),
            42.5,
            Labels::from_pairs(&[("ifName", "Gi0")]).expect("labels"),
        )
        .expect("metric event");
        let sink = SinkConfig::GnmiTarget(GnmiTargetConfig::new(target.local_addr().to_string()));
        emit_metric(&event, &encoder, &sink, None)
            .await
            .expect("emit_metric must deliver to the target");

        assert_eq!(
            target.series_count(),
            1,
            "the event replaced the served series"
        );
        let snapshot = target.snapshot();
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

        drop(held);
        assert_eq!(
            target.series_count(),
            0,
            "the series leaves with the scenario; emit_metric owned nothing"
        );
    }

    /// An event for a path the running target does not serve is refused,
    /// naming the path and the address, and adds nothing.
    #[cfg(feature = "gnmi")]
    #[tokio::test]
    async fn emit_metric_refuses_a_path_the_target_does_not_serve() {
        use crate::sink::gnmi::GnmiTargetConfig;

        let encoder = interface_encoder();
        let held = running_scenario(&encoder, "Gi0").await;
        let target = std::sync::Arc::clone(held.target());
        let addr = target.local_addr();

        let event = MetricEvent::new(
            "in_octets".to_string(),
            42.5,
            Labels::from_pairs(&[("ifName", "Gi1")]).expect("labels"),
        )
        .expect("metric event");
        let sink = SinkConfig::GnmiTarget(GnmiTargetConfig::new(addr.to_string()));
        let err = emit_metric(&event, &encoder, &sink, None)
            .await
            .expect_err("Gi1 is not served");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("interface[name=Gi1]/in-octets") && msg.contains(&addr.to_string()),
            "got {msg}"
        );
        assert_eq!(target.series_count(), 1, "nothing was added");
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

    /// The snmp encoder writing `ifHCInOctets` as a Counter64 of the
    /// interface table.
    #[cfg(feature = "snmp")]
    fn snmp_encoder() -> EncoderConfig {
        use crate::encoder::snmp::{SnmpEncoderConfig, SnmpValueType};

        EncoderConfig::Snmp(SnmpEncoderConfig {
            oids: HashMap::from([(
                "ifHCInOctets".to_string(),
                "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
            )]),
            types: HashMap::from([("ifHCInOctets".to_string(), SnmpValueType::Counter64)]),
            ..SnmpEncoderConfig::default()
        })
    }

    #[cfg(feature = "snmp")]
    fn octets(if_index: &str, value: f64) -> MetricEvent {
        MetricEvent::new(
            "ifHCInOctets".to_string(),
            value,
            Labels::from_pairs(&[("ifIndex", if_index)]).expect("labels"),
        )
        .expect("metric event")
    }

    /// A sink holding an agent and publishing interface 1 stands in for a
    /// running scenario.
    #[cfg(feature = "snmp")]
    async fn running_agent() -> crate::sink::snmp::SnmpAgentSink {
        use crate::sink::snmp::{SnmpAgentConfig, SnmpAgentSink};
        use crate::sink::Sink;

        let mut held = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let event = octets("1", 1.0);
        let mut buf = Vec::new();
        create_encoder(&snmp_encoder())
            .expect("valid encoder")
            .encode_metric(&event, &mut buf)
            .expect("encodes");
        held.write_metric_event(&event, &buf)
            .await
            .expect("the scenario publishes its instance");
        held
    }

    /// `emit_metric` joins the running agent and replaces the value of an
    /// instance it serves, through the metric hook, without owning it.
    #[cfg(feature = "snmp")]
    #[tokio::test]
    async fn emit_metric_replaces_an_instance_the_snmp_agent_serves() {
        use crate::encoder::snmp::oid::Oid;
        use crate::sink::snmp::SnmpAgentConfig;

        let held = running_agent().await;
        let agent = std::sync::Arc::clone(held.agent());
        let instance = Oid::parse("1.3.6.1.2.1.31.1.1.1.6.1").expect("an OID");
        let before = agent.get(&instance);
        let sink = SinkConfig::SnmpAgent(SnmpAgentConfig::new(agent.local_addr().to_string()));
        emit_metric(&octets("1", 42.0), &snmp_encoder(), &sink, None)
            .await
            .expect("emit_metric must deliver to the agent");

        let mut expected = Vec::new();
        create_encoder(&snmp_encoder())
            .expect("valid encoder")
            .encode_metric(&octets("1", 42.0), &mut expected)
            .expect("encodes");
        let after = agent.get(&instance);
        assert_ne!(after, before, "the value changed");
        assert_eq!(
            after,
            crate::sink::snmp::agent::Lookup::Value(bytes::Bytes::copy_from_slice(
                &expected[crate::encoder::snmp::RECORD_HEADER..]
            )),
            "the latest value is the event's"
        );
        assert_eq!(agent.instance_count(), 1, "nothing was added");
        drop(held);
        assert_eq!(
            agent.instance_count(),
            0,
            "the instance leaves with the scenario; emit_metric owned nothing"
        );
    }

    /// An event for an instance the running agent does not serve is
    /// refused, naming the OID and the address, and adds nothing.
    #[cfg(feature = "snmp")]
    #[tokio::test]
    async fn emit_metric_refuses_an_oid_the_snmp_agent_does_not_serve() {
        use crate::sink::snmp::SnmpAgentConfig;

        let held = running_agent().await;
        let addr = held.agent().local_addr();
        let sink = SinkConfig::SnmpAgent(SnmpAgentConfig::new(addr.to_string()));
        let err = emit_metric(&octets("2", 42.0), &snmp_encoder(), &sink, None)
            .await
            .expect_err("interface 2 is not served");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("1.3.6.1.2.1.31.1.1.1.6.2") && msg.contains(&addr.to_string()),
            "{msg}"
        );
        assert_eq!(held.agent().instance_count(), 1, "nothing was added");
    }

    /// An `emit_metric` naming another community than the running agent's is
    /// refused, naming the field and neither community, and the agent's
    /// instance keeps its value.
    #[cfg(feature = "snmp")]
    #[tokio::test]
    async fn emit_metric_with_another_community_is_refused() {
        use crate::encoder::snmp::oid::Oid;
        use crate::sink::snmp::SnmpAgentConfig;

        let held = running_agent().await;
        let agent = std::sync::Arc::clone(held.agent());
        let instance = Oid::parse("1.3.6.1.2.1.31.1.1.1.6.1").expect("an OID");
        let before = agent.get(&instance);
        assert!(
            matches!(before, crate::sink::snmp::agent::Lookup::Value(_)),
            "the agent serves the instance"
        );
        let addr = agent.local_addr().to_string();
        let same = SinkConfig::SnmpAgent(SnmpAgentConfig::new(addr.clone()));
        emit_metric(&octets("1", 7.0), &snmp_encoder(), &same, None)
            .await
            .expect("the agent's own community joins");
        let before = agent.get(&instance);

        let other = SinkConfig::SnmpAgent(SnmpAgentConfig {
            community: "other-community".to_string(),
            ..SnmpAgentConfig::new(addr)
        });
        let err = emit_metric(&octets("1", 42.0), &snmp_encoder(), &other, None)
            .await
            .expect_err("another community is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("community and another community"), "{msg}");
        assert!(
            !msg.contains("other-community") && !msg.contains("public"),
            "{msg}"
        );
        assert_eq!(agent.get(&instance), before, "the value is untouched");
        assert_eq!(agent.instance_count(), 1, "nothing was added");
    }

    /// With no running scenario on the address, `emit_metric` refuses the
    /// event instead of binding an agent of its own.
    #[cfg(feature = "snmp")]
    #[tokio::test]
    async fn emit_metric_without_a_live_snmp_agent_is_a_config_error() {
        use crate::sink::snmp::{SnmpAgentConfig, SnmpAgentSink};

        // A port an agent held and released, so nothing listens on it now.
        let addr = {
            let held = SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0"))
                .await
                .expect("bind an ephemeral port");
            let addr = held.agent().local_addr();
            let closed = held.agent().wait();
            drop(held);
            tokio::time::timeout(std::time::Duration::from_secs(1), closed)
                .await
                .expect("the socket closes within 1 s of the last drop");
            addr
        };
        let sink = SinkConfig::SnmpAgent(SnmpAgentConfig::new(addr.to_string()));
        let err = emit_metric(&octets("1", 1.0), &snmp_encoder(), &sink, None)
            .await
            .expect_err("nothing to join");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(
            err.to_string()
                .contains(&format!("no running scenario listens on {addr}")),
            "{err}"
        );
        tokio::net::UdpSocket::bind(addr)
            .await
            .expect("emit_metric bound nothing on the address");
    }

    /// The snmp encoder and the snmp_agent sink carry metrics only; a log
    /// sent to them is a config error before any sink is built.
    #[cfg(feature = "snmp")]
    #[tokio::test]
    async fn emit_log_rejects_the_snmp_encoder_and_sink() {
        use crate::sink::snmp::SnmpAgentConfig;

        let log = LogEvent::new(
            Severity::Info,
            "probe".to_string(),
            Labels::default(),
            BTreeMap::new(),
        );
        let sink = SinkConfig::SnmpAgent(SnmpAgentConfig::new("127.0.0.1:0"));
        let err = emit_log(&log, &snmp_encoder(), &sink, None)
            .await
            .expect_err("a log on snmp_agent must be refused");
        assert!(matches!(err, SondaError::Config(_)), "got: {err:?}");
        assert!(err.to_string().contains("not logs entries"), "got: {err}");
        let err = emit_log(
            &log,
            &EncoderConfig::JsonLines { precision: None },
            &sink,
            None,
        )
        .await
        .expect_err("the sink alone is refused too");
        assert!(
            err.to_string()
                .contains("snmp_agent sink carries metrics entries only"),
            "{err}"
        );
    }
}
