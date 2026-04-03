//! OTLP/gRPC sink — batches OTLP protobuf data and delivers it via gRPC to an
//! OpenTelemetry Collector.
//!
//! This sink is designed to work with the [`OtlpEncoder`](crate::encoder::otlp::OtlpEncoder),
//! which writes length-prefixed protobuf `Metric` or `LogRecord` bytes. The sink:
//!
//! 1. Receives raw bytes from the encoder via `write()`.
//! 2. Parses each length-prefixed message and accumulates it in a batch.
//! 3. When the batch reaches `batch_size` entries (or on `flush()`), wraps all
//!    accumulated items into an `ExportMetricsServiceRequest` or
//!    `ExportLogsServiceRequest`, and sends it via gRPC unary call.
//!
//! Async gRPC operations are driven by a dedicated single-threaded
//! [`tokio::runtime::Runtime`] stored in the struct, keeping the public
//! [`Sink`] interface fully synchronous. This is the same pattern used by
//! the Kafka sink.
//!
//! Requires the `otlp` feature flag.

use std::marker::PhantomData;

use bytes::Buf;
use prost::Message;
use tokio::runtime::Runtime;
use tonic::client::Grpc;
use tonic::codec::{Codec, Decoder, Encoder as TonicEncoder};
use tonic::transport::Channel;
use tonic::Status;

use crate::encoder::otlp::{
    self, ExportLogsServiceRequest, ExportLogsServiceResponse, ExportMetricsServiceRequest,
    ExportMetricsServiceResponse, InstrumentationScope, KeyValue, LogRecord, Metric, Resource,
    ResourceLogs, ResourceMetrics, ScopeLogs, ScopeMetrics,
};
use crate::sink::Sink;
use crate::SondaError;

/// Default batch size in data point / log record entries (not bytes).
pub const DEFAULT_BATCH_SIZE: usize = 100;

/// The gRPC service path for the OTLP metrics export RPC.
const METRICS_EXPORT_PATH: &str = "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export";

/// The gRPC service path for the OTLP logs export RPC.
const LOGS_EXPORT_PATH: &str = "/opentelemetry.proto.collector.logs.v1.LogsService/Export";

// ---------------------------------------------------------------------------
// Custom ProstCodec for tonic 0.14+
//
// tonic 0.14 removed the built-in ProstCodec. We implement a minimal codec
// that uses prost for encoding/decoding, matching the pattern from earlier
// tonic versions.
// ---------------------------------------------------------------------------

/// A gRPC codec that uses prost for protobuf encoding and decoding.
///
/// Type parameters `T` and `U` are the request and response message types.
#[derive(Debug, Clone)]
struct OtlpCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for OtlpCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for OtlpCodec<T, U>
where
    T: Message + 'static,
    U: Message + Default + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = OtlpProstEncoder<T>;
    type Decoder = OtlpProstDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        OtlpProstEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        OtlpProstDecoder(PhantomData)
    }
}

/// Prost-based encoder for gRPC request messages.
#[derive(Debug)]
struct OtlpProstEncoder<T>(PhantomData<T>);

impl<T: Message + 'static> TonicEncoder for OtlpProstEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("protobuf encode error: {e}")))
    }
}

/// Prost-based decoder for gRPC response messages.
#[derive(Debug)]
struct OtlpProstDecoder<T>(PhantomData<T>);

impl<T: Message + Default + 'static> Decoder for OtlpProstDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let buf = src.copy_to_bytes(src.remaining());
        if buf.is_empty() {
            return Ok(None);
        }
        T::decode(buf)
            .map(Some)
            .map_err(|e| Status::internal(format!("protobuf decode error: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Signal type
// ---------------------------------------------------------------------------

/// Selects which OTLP signal type the sink handles.
///
/// Determines both the gRPC path and the parsing/batching strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Deserialize))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
pub enum OtlpSignalType {
    /// Metric signal — data is parsed as OTLP `Metric` messages and sent
    /// to the `MetricsService/Export` gRPC endpoint.
    Metrics,
    /// Log signal — data is parsed as OTLP `LogRecord` messages and sent
    /// to the `LogsService/Export` gRPC endpoint.
    Logs,
}

// ---------------------------------------------------------------------------
// Sink implementation
// ---------------------------------------------------------------------------

/// Delivers OTLP protobuf telemetry to an OpenTelemetry Collector via gRPC.
///
/// The sink accumulates encoded data points in an internal batch. When the
/// batch reaches `batch_size` entries, or when `flush()` is called, the batch
/// is wrapped in the appropriate OTLP export request and sent via gRPC unary
/// call to the configured endpoint.
///
/// Uses a private single-threaded [`Runtime`] to drive async tonic calls,
/// keeping the [`Sink`] trait interface fully synchronous.
pub struct OtlpGrpcSink {
    /// Tokio runtime used to drive async tonic calls synchronously.
    runtime: Runtime,
    /// The gRPC channel (connection) to the OTLP endpoint.
    channel: Channel,
    /// Accumulated metrics waiting to be sent.
    metric_batch: Vec<Metric>,
    /// Accumulated log records waiting to be sent.
    log_batch: Vec<LogRecord>,
    /// Flush threshold in number of data points / log records.
    batch_size: usize,
    /// Whether this sink handles metrics or logs.
    signal_type: OtlpSignalType,
    /// Resource attributes derived from scenario labels.
    resource_attrs: Vec<KeyValue>,
    /// The endpoint URL string (stored for error messages).
    endpoint: String,
}

impl OtlpGrpcSink {
    /// Create a new `OtlpGrpcSink` connected to the given OTLP endpoint.
    ///
    /// # Arguments
    ///
    /// - `endpoint` — the gRPC endpoint URL, e.g. `"http://localhost:4317"`.
    /// - `signal_type` — whether to send metrics or logs.
    /// - `batch_size` — flush threshold in number of data points / log records.
    ///   Use [`DEFAULT_BATCH_SIZE`] if no override is needed.
    /// - `resource_attrs` — key-value pairs for the OTLP `Resource` (typically
    ///   from scenario labels).
    ///
    /// # Errors
    ///
    /// Returns [`SondaError::Sink`] if:
    /// - The tokio runtime cannot be created.
    /// - The endpoint URL cannot be parsed.
    /// - The gRPC connection cannot be established.
    pub fn new(
        endpoint: &str,
        signal_type: OtlpSignalType,
        batch_size: usize,
        resource_attrs: Vec<KeyValue>,
    ) -> Result<Self, SondaError> {
        // Build a minimal single-threaded tokio runtime.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                std::io::Error::other(format!(
                    "otlp grpc sink: failed to build tokio runtime for '{}': {}",
                    endpoint, e
                ))
            })
            .map_err(SondaError::Sink)?;

        let endpoint_str = endpoint.to_owned();

        // Connect to the gRPC endpoint.
        let channel = runtime
            .block_on(async {
                Channel::from_shared(endpoint_str.clone())
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "otlp grpc sink: invalid endpoint '{}': {}",
                            endpoint_str, e
                        ))
                    })?
                    .connect()
                    .await
                    .map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            format!(
                                "otlp grpc sink: failed to connect to '{}': {}",
                                endpoint_str, e
                            ),
                        )
                    })
            })
            .map_err(SondaError::Sink)?;

        Ok(Self {
            runtime,
            channel,
            metric_batch: Vec::with_capacity(batch_size),
            log_batch: Vec::with_capacity(batch_size),
            batch_size,
            signal_type,
            resource_attrs,
            endpoint: endpoint.to_owned(),
        })
    }

    /// Build the OTLP `Resource` from the stored resource attributes.
    fn build_resource(&self) -> Resource {
        Resource {
            attributes: self.resource_attrs.clone(),
        }
    }

    /// Build the standard instrumentation scope for Sonda.
    fn build_scope() -> InstrumentationScope {
        InstrumentationScope {
            name: "sonda".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Flush the metric batch as an `ExportMetricsServiceRequest` via gRPC.
    fn flush_metrics(&mut self) -> Result<(), SondaError> {
        if self.metric_batch.is_empty() {
            return Ok(());
        }

        let metrics =
            std::mem::replace(&mut self.metric_batch, Vec::with_capacity(self.batch_size));

        let request = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(self.build_resource()),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(Self::build_scope()),
                    metrics,
                }],
            }],
        };

        self.send_grpc_unary::<ExportMetricsServiceRequest, ExportMetricsServiceResponse>(
            request,
            METRICS_EXPORT_PATH,
        )
    }

    /// Flush the log batch as an `ExportLogsServiceRequest` via gRPC.
    fn flush_logs(&mut self) -> Result<(), SondaError> {
        if self.log_batch.is_empty() {
            return Ok(());
        }

        let log_records =
            std::mem::replace(&mut self.log_batch, Vec::with_capacity(self.batch_size));

        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(self.build_resource()),
                scope_logs: vec![ScopeLogs {
                    scope: Some(Self::build_scope()),
                    log_records,
                }],
            }],
        };

        self.send_grpc_unary::<ExportLogsServiceRequest, ExportLogsServiceResponse>(
            request,
            LOGS_EXPORT_PATH,
        )
    }

    /// Send a gRPC unary call using the custom prost codec.
    fn send_grpc_unary<T, U>(&mut self, request: T, path: &'static str) -> Result<(), SondaError>
    where
        T: Message + 'static,
        U: Message + Default + 'static,
    {
        let channel = self.channel.clone();
        let endpoint = self.endpoint.clone();

        let result = self.runtime.block_on(async {
            let mut client = Grpc::new(channel);
            client.ready().await.map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("otlp grpc sink: service not ready at '{}': {}", endpoint, e),
                )
            })?;

            let grpc_path = http::uri::PathAndQuery::from_static(path);
            let codec: OtlpCodec<T, U> = OtlpCodec::default();
            let tonic_request = tonic::Request::new(request);

            client
                .unary(tonic_request, grpc_path, codec)
                .await
                .map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("otlp grpc sink: gRPC call to '{}' failed: {}", endpoint, e),
                    )
                })?;

            Ok::<(), std::io::Error>(())
        });

        result.map_err(SondaError::Sink)
    }
}

impl Sink for OtlpGrpcSink {
    /// Accept length-prefixed OTLP protobuf bytes from the encoder.
    ///
    /// Parses each message from the data and adds it to the internal batch.
    /// When the batch reaches `batch_size` entries, an automatic flush is triggered.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        match self.signal_type {
            OtlpSignalType::Metrics => {
                let metrics = otlp::parse_length_prefixed_metrics(data)?;
                self.metric_batch.extend(metrics);
                if self.metric_batch.len() >= self.batch_size {
                    self.flush_metrics()?;
                }
            }
            OtlpSignalType::Logs => {
                let records = otlp::parse_length_prefixed_log_records(data)?;
                self.log_batch.extend(records);
                if self.log_batch.len() >= self.batch_size {
                    self.flush_logs()?;
                }
            }
        }
        Ok(())
    }

    /// Flush any remaining buffered data to the OTLP endpoint.
    ///
    /// Safe to call multiple times. Returns `Ok(())` immediately if the batch
    /// is empty.
    fn flush(&mut self) -> Result<(), SondaError> {
        match self.signal_type {
            OtlpSignalType::Metrics => self.flush_metrics(),
            OtlpSignalType::Logs => self.flush_logs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::otlp::{
        self, any_value, metric, number_data_point, AnyValue, Gauge, Metric, NumberDataPoint,
    };
    use crate::sink::SinkConfig;

    // -----------------------------------------------------------------------
    // Constants
    // -----------------------------------------------------------------------

    #[test]
    fn default_batch_size_is_100() {
        assert_eq!(DEFAULT_BATCH_SIZE, 100);
    }

    // -----------------------------------------------------------------------
    // OtlpSignalType
    // -----------------------------------------------------------------------

    #[test]
    fn signal_type_is_cloneable_and_debuggable() {
        let st = OtlpSignalType::Metrics;
        let cloned = st;
        assert_eq!(cloned, OtlpSignalType::Metrics);
        let s = format!("{st:?}");
        assert!(s.contains("Metrics"));
    }

    #[cfg(feature = "config")]
    #[test]
    fn signal_type_deserializes_metrics() {
        let json = "\"metrics\"";
        let st: OtlpSignalType = serde_json::from_str(json).expect("deser ok");
        assert_eq!(st, OtlpSignalType::Metrics);
    }

    #[cfg(feature = "config")]
    #[test]
    fn signal_type_deserializes_logs() {
        let json = "\"logs\"";
        let st: OtlpSignalType = serde_json::from_str(json).expect("deser ok");
        assert_eq!(st, OtlpSignalType::Logs);
    }

    // -----------------------------------------------------------------------
    // Send + Sync contract
    // -----------------------------------------------------------------------

    #[test]
    fn otlp_grpc_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OtlpGrpcSink>();
    }

    // -----------------------------------------------------------------------
    // SinkConfig deserialization
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_otlp_grpc_deserializes_with_all_fields() {
        let yaml = r#"
type: otlp_grpc
endpoint: "http://localhost:4317"
signal_type: metrics
batch_size: 50
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("deser ok");
        match config {
            SinkConfig::OtlpGrpc {
                endpoint,
                signal_type,
                batch_size,
            } => {
                assert_eq!(endpoint, "http://localhost:4317");
                assert_eq!(signal_type, OtlpSignalType::Metrics);
                assert_eq!(batch_size, Some(50));
            }
            other => panic!("expected OtlpGrpc, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_otlp_grpc_batch_size_is_optional() {
        let yaml = r#"
type: otlp_grpc
endpoint: "http://localhost:4317"
signal_type: logs
"#;
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("deser ok");
        match config {
            SinkConfig::OtlpGrpc {
                batch_size,
                signal_type,
                ..
            } => {
                assert!(batch_size.is_none());
                assert_eq!(signal_type, OtlpSignalType::Logs);
            }
            other => panic!("expected OtlpGrpc, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_otlp_grpc_requires_endpoint() {
        let yaml = "type: otlp_grpc\nsignal_type: metrics";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "otlp_grpc without endpoint should fail deserialization"
        );
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_otlp_grpc_requires_signal_type() {
        let yaml = "type: otlp_grpc\nendpoint: \"http://localhost:4317\"";
        let result: Result<SinkConfig, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "otlp_grpc without signal_type should fail deserialization"
        );
    }

    #[test]
    fn sink_config_otlp_grpc_is_cloneable_and_debuggable() {
        let config = SinkConfig::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            signal_type: OtlpSignalType::Metrics,
            batch_size: Some(100),
        };
        let cloned = config.clone();
        let s = format!("{cloned:?}");
        assert!(s.contains("OtlpGrpc"));
        assert!(s.contains("4317"));
    }

    // -----------------------------------------------------------------------
    // Request construction helpers (unit tests without network)
    // -----------------------------------------------------------------------

    /// Verify that `ExportMetricsServiceRequest` wrapping produces valid protobuf.
    #[test]
    fn export_metrics_request_wraps_metrics_correctly() {
        let metric = Metric {
            name: "test_gauge".to_string(),
            description: String::new(),
            unit: String::new(),
            data: Some(metric::Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    attributes: vec![],
                    time_unix_nano: 1_700_000_000_000_000_000,
                    value: Some(number_data_point::Value::AsDouble(42.0)),
                }],
            })),
        };

        let attrs = vec![otlp::KeyValue {
            key: "service.name".to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue("sonda".to_string())),
            }),
        }];

        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource { attributes: attrs }),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(InstrumentationScope {
                        name: "sonda".to_string(),
                        version: "test".to_string(),
                    }),
                    metrics: vec![metric],
                }],
            }],
        };

        // Roundtrip through protobuf
        let mut buf = Vec::new();
        req.encode(&mut buf).expect("encode");
        let decoded = ExportMetricsServiceRequest::decode(buf.as_slice()).expect("decode");
        assert_eq!(decoded.resource_metrics.len(), 1);
        assert_eq!(
            decoded.resource_metrics[0].scope_metrics[0].metrics.len(),
            1
        );
        assert_eq!(
            decoded.resource_metrics[0].scope_metrics[0].metrics[0].name,
            "test_gauge"
        );
    }

    /// Verify that `ExportLogsServiceRequest` wrapping produces valid protobuf.
    #[test]
    fn export_logs_request_wraps_log_records_correctly() {
        let record = otlp::LogRecord {
            time_unix_nano: 1_700_000_000_000_000_000,
            severity_number: 9,
            severity_text: "INFO".to_string(),
            body: Some(AnyValue {
                value: Some(any_value::Value::StringValue("hello".to_string())),
            }),
            attributes: vec![],
        };

        let req = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource { attributes: vec![] }),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope {
                        name: "sonda".to_string(),
                        version: "test".to_string(),
                    }),
                    log_records: vec![record],
                }],
            }],
        };

        let mut buf = Vec::new();
        req.encode(&mut buf).expect("encode");
        let decoded = ExportLogsServiceRequest::decode(buf.as_slice()).expect("decode");
        assert_eq!(decoded.resource_logs.len(), 1);
        assert_eq!(decoded.resource_logs[0].scope_logs[0].log_records.len(), 1);
        assert_eq!(
            decoded.resource_logs[0].scope_logs[0].log_records[0].severity_text,
            "INFO"
        );
    }

    // -----------------------------------------------------------------------
    // Construction failure: unreachable endpoint
    // -----------------------------------------------------------------------

    /// Connecting to a port where no OTLP collector is listening must return a
    /// SondaError::Sink.
    #[test]
    #[ignore = "requires network timeout; run with --ignored when desired"]
    fn new_with_unreachable_endpoint_returns_sink_error() {
        let result = OtlpGrpcSink::new(
            "http://127.0.0.1:1",
            OtlpSignalType::Metrics,
            DEFAULT_BATCH_SIZE,
            vec![],
        );
        match result {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("127.0.0.1:1") || msg.contains("otlp"),
                    "error should reference the endpoint: {msg}"
                );
            }
            Ok(_) => panic!("construction must fail when endpoint is unreachable"),
        }
    }

    /// An invalid endpoint URL (not parseable) should return an error.
    #[test]
    fn new_with_invalid_endpoint_returns_error() {
        let result = OtlpGrpcSink::new(
            "not a url",
            OtlpSignalType::Metrics,
            DEFAULT_BATCH_SIZE,
            vec![],
        );
        assert!(result.is_err(), "invalid endpoint URL must be rejected");
    }

    // -----------------------------------------------------------------------
    // Full scenario YAML: otlp_grpc sink variant
    // -----------------------------------------------------------------------

    #[cfg(feature = "config")]
    #[test]
    fn scenario_yaml_with_otlp_metrics_deserializes() {
        use crate::config::ScenarioConfig;
        use crate::encoder::EncoderConfig;

        let yaml = r#"
name: otlp_test
rate: 10.0
generator:
  type: constant
  value: 1.0
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: metrics
"#;
        let config: ScenarioConfig = serde_yaml_ng::from_str(yaml).expect("deser ok");
        assert_eq!(config.name, "otlp_test");
        assert!(matches!(config.encoder, EncoderConfig::Otlp));
        assert!(matches!(
            config.sink,
            SinkConfig::OtlpGrpc {
                ref endpoint,
                signal_type: OtlpSignalType::Metrics,
                ..
            } if endpoint == "http://localhost:4317"
        ));
    }

    #[cfg(feature = "config")]
    #[test]
    fn scenario_yaml_with_otlp_logs_deserializes() {
        use crate::config::LogScenarioConfig;
        use crate::encoder::EncoderConfig;

        let yaml = r#"
name: otlp_logs_test
rate: 5.0
generator:
  type: template
  templates:
    - message: "Request processed"
encoder:
  type: otlp
sink:
  type: otlp_grpc
  endpoint: "http://localhost:4317"
  signal_type: logs
  batch_size: 50
"#;
        let config: LogScenarioConfig = serde_yaml_ng::from_str(yaml).expect("deser ok");
        assert_eq!(config.name, "otlp_logs_test");
        assert!(matches!(config.encoder, EncoderConfig::Otlp));
        match &config.sink {
            SinkConfig::OtlpGrpc {
                endpoint,
                signal_type,
                batch_size,
            } => {
                assert_eq!(endpoint, "http://localhost:4317");
                assert_eq!(*signal_type, OtlpSignalType::Logs);
                assert_eq!(*batch_size, Some(50));
            }
            other => panic!("expected OtlpGrpc, got {other:?}"),
        }
    }
}
