//! POST /events — synchronous single-event emission.

use std::collections::HashMap;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sonda_core::emit::{emit_log, emit_metric};
use sonda_core::encoder::EncoderConfig;
use sonda_core::model::log::{LogEvent, Severity};
use sonda_core::model::metric::{Labels, MetricEvent};
use sonda_core::sink::SinkConfig;
use sonda_core::SondaError;
use tracing::info;

use crate::routes::sink_warnings::{collect_warnings_for_sink, log_warnings};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct LogPayload {
    pub severity: Severity,
    pub message: String,
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricPayload {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "signal_type")]
pub enum EventRequest {
    #[serde(rename = "logs")]
    Logs {
        #[serde(default)]
        labels: HashMap<String, String>,
        log: LogPayload,
        encoder: EncoderConfig,
        sink: SinkConfig,
    },
    #[serde(rename = "metrics")]
    Metrics {
        #[serde(default)]
        labels: HashMap<String, String>,
        metric: MetricPayload,
        encoder: EncoderConfig,
        sink: SinkConfig,
    },
}

#[derive(Debug, Serialize)]
pub struct EventAck {
    pub sent: bool,
    pub signal_type: &'static str,
    pub latency_ms: u128,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

pub async fn post_events(State(_state): State<AppState>, body: axum::body::Bytes) -> Response {
    let req: EventRequest = match serde_json::from_slice::<EventRequest>(&body) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("invalid event body: {e}")),
    };

    let mut warnings: Vec<String> = Vec::new();
    let warning_label = match &req {
        EventRequest::Logs { .. } => "events.logs",
        EventRequest::Metrics { .. } => "events.metrics",
    };
    match &req {
        EventRequest::Logs { sink, .. } | EventRequest::Metrics { sink, .. } => {
            collect_warnings_for_sink(sink, warning_label, &mut warnings);
        }
    }
    log_warnings("POST /events", &warnings);

    let started = Instant::now();
    let (signal_type, sink_type, emit_result) = match req {
        EventRequest::Logs {
            labels,
            log,
            encoder,
            sink,
        } => {
            let event = match build_log_event(log, &labels) {
                Ok(e) => e,
                Err(e) => return error_response(e),
            };
            let sink_type = sink_kind(&sink);
            let labels_for_sink = (!labels.is_empty()).then_some(labels);
            let result =
                run_blocking(move || emit_log(&event, &encoder, &sink, labels_for_sink.as_ref()))
                    .await;
            ("logs", sink_type, result)
        }
        EventRequest::Metrics {
            labels,
            metric,
            encoder,
            sink,
        } => {
            let event = match build_metric_event(metric, &labels) {
                Ok(e) => e,
                Err(e) => return error_response(e),
            };
            let sink_type = sink_kind(&sink);
            let labels_for_sink = (!labels.is_empty()).then_some(labels);
            let result = run_blocking(move || {
                emit_metric(&event, &encoder, &sink, labels_for_sink.as_ref())
            })
            .await;
            ("metrics", sink_type, result)
        }
    };
    let latency_ms = started.elapsed().as_millis();

    match emit_result {
        Ok(()) => {
            info!(
                signal_type = signal_type,
                sink_type = sink_type,
                latency_ms = latency_ms,
                result = "ok",
                "POST /events: event delivered"
            );
            (
                StatusCode::OK,
                Json(EventAck {
                    sent: true,
                    signal_type,
                    latency_ms,
                    warnings,
                }),
            )
                .into_response()
        }
        Err(err) => {
            info!(
                signal_type = signal_type,
                sink_type = sink_type,
                latency_ms = latency_ms,
                result = "error",
                error = %err,
                "POST /events: event delivery failed"
            );
            error_response(err)
        }
    }
}

fn build_log_event(
    log: LogPayload,
    labels: &HashMap<String, String>,
) -> Result<LogEvent, SondaError> {
    let labels = labels_from_map(labels)?;
    let fields: std::collections::BTreeMap<String, String> = log.fields.into_iter().collect();
    Ok(LogEvent::new(log.severity, log.message, labels, fields))
}

fn build_metric_event(
    metric: MetricPayload,
    labels: &HashMap<String, String>,
) -> Result<MetricEvent, SondaError> {
    let labels = labels_from_map(labels)?;
    MetricEvent::new(metric.name, metric.value, labels)
}

fn labels_from_map(map: &HashMap<String, String>) -> Result<Labels, SondaError> {
    if map.is_empty() {
        return Ok(Labels::default());
    }
    let pairs: Vec<(&str, &str)> = map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Labels::from_pairs(&pairs)
}

async fn run_blocking<F>(f: F) -> Result<(), SondaError>
where
    F: FnOnce() -> Result<(), SondaError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(_join_err) => Err(SondaError::Runtime(
            sonda_core::RuntimeError::ThreadPanicked,
        )),
    }
}

fn error_response(err: SondaError) -> Response {
    match err {
        SondaError::Config(e) => unprocessable(format!("{e}")),
        SondaError::Sink(e) => bad_gateway(format!("sink error: {e}")),
        SondaError::Encoder(e) => internal_error(format!("encoder error: {e}")),
        SondaError::Generator(e) => internal_error(format!("generator error: {e}")),
        SondaError::Runtime(e) => internal_error(format!("runtime error: {e}")),
        _ => internal_error("unexpected error variant"),
    }
}

fn sink_kind(sink: &SinkConfig) -> &'static str {
    match sink {
        SinkConfig::Stdout => "stdout",
        SinkConfig::File { .. } => "file",
        SinkConfig::Tcp { .. } => "tcp",
        SinkConfig::Udp { .. } => "udp",
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { .. } => "http_push",
        #[cfg(feature = "http")]
        SinkConfig::Loki { .. } => "loki",
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { .. } => "remote_write",
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { .. } => "kafka",
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc { .. } => "otlp_grpc",
        _ => "other",
    }
}

fn bad_request(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "bad_request", "detail": detail.to_string() });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

fn unprocessable(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "unprocessable_entity", "detail": detail.to_string() });
    (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
}

fn bad_gateway(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "bad_gateway", "detail": detail.to_string() });
    (StatusCode::BAD_GATEWAY, Json(body)).into_response()
}

fn internal_error(detail: impl std::fmt::Display) -> Response {
    let body = json!({ "error": "internal_server_error", "detail": detail.to_string() });
    (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_logs_branch() {
        let payload = serde_json::json!({
            "signal_type": "logs",
            "labels": {"event": "deploy_start"},
            "log": {"severity": "info", "message": "go", "fields": {}},
            "encoder": {"type": "json_lines"},
            "sink": {"type": "stdout"},
        });
        let req: EventRequest = serde_json::from_value(payload).expect("must parse");
        match req {
            EventRequest::Logs { log, .. } => {
                assert_eq!(log.message, "go");
                assert_eq!(log.severity, Severity::Info);
            }
            _ => panic!("expected Logs branch"),
        }
    }

    #[test]
    fn deserializes_metrics_branch() {
        let payload = serde_json::json!({
            "signal_type": "metrics",
            "labels": {},
            "metric": {"name": "x", "value": 1.0},
            "encoder": {"type": "prometheus_text"},
            "sink": {"type": "stdout"},
        });
        let req: EventRequest = serde_json::from_value(payload).expect("must parse");
        match req {
            EventRequest::Metrics { metric, .. } => {
                assert_eq!(metric.name, "x");
                assert_eq!(metric.value, 1.0);
            }
            _ => panic!("expected Metrics branch"),
        }
    }

    #[test]
    fn unknown_signal_type_fails_to_deserialize() {
        let payload = serde_json::json!({
            "signal_type": "traces",
            "encoder": {"type": "json_lines"},
            "sink": {"type": "stdout"},
        });
        let err = serde_json::from_value::<EventRequest>(payload)
            .expect_err("unknown signal_type must error");
        let msg = err.to_string();
        assert!(
            msg.contains("traces") || msg.to_lowercase().contains("variant"),
            "error must mention the bad tag, got: {msg}"
        );
    }

    #[test]
    fn missing_log_message_fails_to_deserialize() {
        let payload = serde_json::json!({
            "signal_type": "logs",
            "log": {"severity": "info"},
            "encoder": {"type": "json_lines"},
            "sink": {"type": "stdout"},
        });
        let err = serde_json::from_value::<EventRequest>(payload)
            .expect_err("missing message must error");
        assert!(
            err.to_string().to_lowercase().contains("message"),
            "error must mention the missing field, got: {err}"
        );
    }

    #[test]
    fn error_response_maps_config_to_422() {
        let err = SondaError::Config(sonda_core::ConfigError::InvalidValue("bad".to_string()));
        let resp = error_response(err);
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn error_response_maps_sink_to_502() {
        let err = SondaError::Sink(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "nope",
        ));
        let resp = error_response(err);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn error_response_maps_runtime_to_500() {
        let err = SondaError::Runtime(sonda_core::RuntimeError::ThreadPanicked);
        let resp = error_response(err);
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn sink_kind_tags_match_yaml_type_names() {
        assert_eq!(sink_kind(&SinkConfig::Stdout), "stdout");
        assert_eq!(
            sink_kind(&SinkConfig::Tcp {
                address: "x:1".into(),
                retry: None,
            }),
            "tcp"
        );
        assert_eq!(
            sink_kind(&SinkConfig::Udp {
                address: "x:1".into(),
            }),
            "udp"
        );
    }

    #[test]
    fn labels_from_empty_map_returns_default() {
        let map: HashMap<String, String> = HashMap::new();
        let labels = labels_from_map(&map).expect("empty must succeed");
        assert!(labels.is_empty());
    }

    #[test]
    fn labels_from_map_rejects_invalid_keys() {
        let mut map = HashMap::new();
        map.insert("1bad".to_string(), "value".to_string());
        let err = labels_from_map(&map).expect_err("invalid key must fail");
        assert!(matches!(err, SondaError::Config(_)));
    }
}
