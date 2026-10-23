//! `GET /gnmi/targets`: the gnmi targets this process serves, with their
//! series and subscribers.

use std::time::SystemTime;

use axum::Json;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sonda_core::sink::gnmi::{registry, GnmiTarget, SubscriberInfo};

/// Body of `GET /gnmi/targets`.
#[derive(Debug, Serialize)]
pub struct TargetsResponse {
    /// Every live target, sorted by listen address.
    pub targets: Vec<TargetBody>,
}

/// One target.
#[derive(Debug, Serialize)]
pub struct TargetBody {
    /// The address the target listens on, with an ephemeral port resolved.
    pub listen: String,
    /// Whether the target serves TLS.
    pub tls: bool,
    /// Whether every request must carry the target's credentials.
    pub credentials: bool,
    /// Distinct series paths the target serves.
    pub series: usize,
    /// Subscriptions the target serves now.
    pub subscribers: usize,
    /// The most subscriptions the target serves at once.
    pub max_subscribers: usize,
    /// Get requests the target has received.
    pub get_requests: u64,
    /// Subscriptions the target has ended as stalled.
    pub stalled_subscribers_total: u64,
    /// Each subscription, in the order they took their place.
    pub subscriber_list: Vec<SubscriberBody>,
}

/// One subscription.
#[derive(Debug, Serialize)]
pub struct SubscriberBody {
    /// The client's address, `null` when the connection reports none.
    pub peer: Option<String>,
    /// `ONCE`, `POLL`, or `STREAM/` and the subscriptions' mode.
    pub mode: &'static str,
    /// `PROTO` or `JSON_IETF`.
    pub encoding: &'static str,
    /// When the subscription took its place, RFC 3339 UTC.
    pub since: String,
    /// When a response last left its queue, RFC 3339 UTC; `null` before the
    /// first.
    pub last_drained: Option<String>,
    /// Whether it is being ended as stalled.
    pub stalled: bool,
}

/// `GET /gnmi/targets` → 200 with every live target in the process.
pub async fn list_targets() -> Json<TargetsResponse> {
    Json(TargetsResponse {
        targets: registry()
            .targets()
            .iter()
            .map(|t| target_body(t))
            .collect(),
    })
}

fn target_body(target: &GnmiTarget) -> TargetBody {
    TargetBody {
        listen: target.local_addr().to_string(),
        tls: target.tls(),
        credentials: target.requires_credentials(),
        series: target.series_count(),
        subscribers: target.subscriber_count(),
        max_subscribers: target.max_subscribers(),
        get_requests: target.get_requests(),
        stalled_subscribers_total: target.stalled_subscribers_total(),
        subscriber_list: target.subscribers().iter().map(subscriber_body).collect(),
    }
}

fn subscriber_body(info: &SubscriberInfo) -> SubscriberBody {
    SubscriberBody {
        peer: info.peer.map(|peer| peer.to_string()),
        mode: info.mode,
        encoding: info.encoding,
        since: rfc3339(info.since),
        last_drained: info.last_drained.map(rfc3339),
        stalled: info.stalled,
    }
}

/// `time` as RFC 3339 UTC to the second, e.g. `2026-10-22T10:00:00Z`.
fn rfc3339(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use std::net::SocketAddr;
    use std::time::Duration;

    use http_body_util::BodyExt;
    use hyper::{Request, StatusCode};
    use sonda_core::encoder::gnmi::path::parse_client_path;
    use sonda_core::encoder::gnmi::proto::{
        self, subscribe_request, subscribe_response, subscription_list, SubscriptionMode,
    };
    use sonda_core::ScenarioHandle;
    use tokio_stream::StreamExt;
    use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
    use tonic::Status;
    use tower::ServiceExt;

    use super::*;
    use crate::routes::router;
    use crate::state::AppState;

    const PATIENCE: Duration = Duration::from_secs(5);

    /// A tonic codec over prost messages, for the test client.
    struct ProstCodec<T, U>(PhantomData<(T, U)>);

    impl<T, U> Default for ProstCodec<T, U> {
        fn default() -> Self {
            Self(PhantomData)
        }
    }

    impl<T, U> Codec for ProstCodec<T, U>
    where
        T: prost::Message + Send + 'static,
        U: prost::Message + Default + Send + 'static,
    {
        type Encode = T;
        type Decode = U;
        type Encoder = ProstEncoder<T>;
        type Decoder = ProstDecoder<U>;

        fn encoder(&mut self) -> Self::Encoder {
            ProstEncoder(PhantomData)
        }

        fn decoder(&mut self) -> Self::Decoder {
            ProstDecoder(PhantomData)
        }
    }

    struct ProstEncoder<T>(PhantomData<T>);

    impl<T: prost::Message> Encoder for ProstEncoder<T> {
        type Item = T;
        type Error = Status;

        fn encode(&mut self, item: T, buf: &mut EncodeBuf<'_>) -> Result<(), Status> {
            item.encode(buf)
                .map_err(|e| Status::internal(e.to_string()))
        }
    }

    struct ProstDecoder<U>(PhantomData<U>);

    impl<U: prost::Message + Default> Decoder for ProstDecoder<U> {
        type Item = U;
        type Error = Status;

        fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
            U::decode(buf)
                .map(Some)
                .map_err(|e| Status::internal(e.to_string()))
        }
    }

    /// An open STREAM SAMPLE subscription to `/interfaces` on `addr`.
    async fn subscribe(addr: SocketAddr) -> tonic::Streaming<proto::SubscribeResponse> {
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("a valid uri")
            .connect()
            .await
            .expect("connect to the target");
        let list = proto::SubscriptionList {
            prefix: None,
            subscription: vec![proto::Subscription {
                path: Some(parse_client_path("/interfaces").expect("a valid path")),
                mode: SubscriptionMode::Sample as i32,
                sample_interval: Duration::from_secs(1).as_nanos() as u64,
                ..Default::default()
            }],
            mode: subscription_list::Mode::Stream as i32,
            encoding: proto::Encoding::Proto as i32,
            updates_only: false,
        };
        let request = proto::SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(list)),
        };
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.expect("the channel is ready");
        grpc.streaming(
            tonic::Request::new(tokio_stream::iter([request])),
            axum::http::uri::PathAndQuery::from_static("/gnmi.gNMI/Subscribe"),
            ProstCodec::<proto::SubscribeRequest, proto::SubscribeResponse>::default(),
        )
        .await
        .expect("subscribed")
        .into_inner()
    }

    /// `GET /gnmi/targets` through the router.
    async fn get_targets(state: &AppState) -> serde_json::Value {
        let request = Request::builder()
            .uri("/gnmi/targets")
            .body(axum::body::Body::empty())
            .expect("request");
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("infallible");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("a JSON body")
    }

    /// Poll `GET /gnmi/targets` until `done` holds of its body; the last
    /// body read.
    async fn targets_until(
        state: &AppState,
        mut done: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let body = get_targets(state).await;
            if done(&body) || tokio::time::Instant::now() >= deadline {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Stop and join every scenario in `state`.
    async fn stop_all(state: &AppState) {
        let handles: Vec<ScenarioHandle> =
            state.scenarios.write().drain().map(|(_, h)| h).collect();
        for mut handle in handles {
            handle.stop();
            let _ = handle.join_async(Some(PATIENCE)).await;
        }
    }

    /// Two scenarios started through `POST /scenarios`, each with a
    /// `gnmi_target` on `127.0.0.1:0`, the second over TLS with
    /// credentials. The route lists both in address order with their
    /// settings, series and subscriber counts; a subscribed tonic client
    /// appears on the plain target with its address, and is gone after it
    /// disconnects; both targets are gone once the scenarios stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_targets_route_lists_each_target_and_its_subscribers() {
        let _registry = crate::routes::GNMI_REGISTRY.lock().await;
        assert!(
            registry().targets().is_empty(),
            "the premise: no target is running"
        );

        let certs = tempfile::tempdir().expect("a temporary directory");
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("a cert");
        let cert = certs.path().join("server.pem");
        let key = certs.path().join("server-key.pem");
        std::fs::write(&cert, certified.cert.pem()).expect("write the cert");
        std::fs::write(&key, certified.key_pair.serialize_pem()).expect("write the key");

        let yaml = format!(
            "\
version: 2
kind: runnable
defaults:
  rate: 20
  encoder:
    type: gnmi
    path: \"/interfaces/interface[name={{ifName}}]/{{name}}\"
scenarios:
  - signal_type: metrics
    name: in_octets
    generator: {{ type: constant, value: 1.0 }}
    labels: {{ device: plain, ifName: eth0 }}
    sink:
      type: gnmi_target
      listen: \"127.0.0.1:0\"
      max_subscribers: 3
  - signal_type: metrics
    name: in_octets
    generator: {{ type: constant, value: 1.0 }}
    labels: {{ device: secured, ifName: eth0 }}
    sink:
      type: gnmi_target
      listen: \"127.0.0.1:0\"
      tls: {{ cert: \"{}\", key: \"{}\" }}
      credentials: {{ username: sonda, password: route-test }}
",
            cert.display(),
            key.display()
        );
        let state = AppState::new();
        let request = Request::builder()
            .method("POST")
            .uri("/scenarios")
            .header("content-type", "application/x-yaml")
            .body(axum::body::Body::from(yaml))
            .expect("request");
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("infallible");
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = targets_until(&state, |body| {
            body["targets"].as_array().is_some_and(|targets| {
                targets.len() == 2 && targets.iter().all(|t| t["series"] == 1)
            })
        })
        .await;
        let targets = body["targets"].as_array().expect("a targets array");
        assert_eq!(targets.len(), 2, "both targets: {body}");
        let listens: Vec<SocketAddr> = targets
            .iter()
            .map(|t| {
                t["listen"]
                    .as_str()
                    .expect("listen")
                    .parse()
                    .expect("an address")
            })
            .collect();
        assert!(listens[0] < listens[1], "address order: {listens:?}");
        let (plain, secured) = if targets[0]["tls"] == false {
            (&targets[0], &targets[1])
        } else {
            (&targets[1], &targets[0])
        };
        assert_eq!(
            (&plain["tls"], &plain["credentials"]),
            (&serde_json::json!(false), &serde_json::json!(false))
        );
        assert_eq!(
            (&secured["tls"], &secured["credentials"]),
            (&serde_json::json!(true), &serde_json::json!(true))
        );
        assert_eq!(plain["max_subscribers"], 3);
        assert_eq!(secured["max_subscribers"], 16);
        for target in targets {
            assert_eq!(target["series"], 1, "{target}");
            assert_eq!(target["subscribers"], 0, "{target}");
            assert_eq!(target["subscriber_list"], serde_json::json!([]), "{target}");
            assert_eq!(target["get_requests"], 0, "{target}");
            assert_eq!(target["stalled_subscribers_total"], 0, "{target}");
        }

        let plain_listen = plain["listen"].as_str().expect("listen").to_string();
        let mut client = subscribe(plain_listen.parse().expect("an address")).await;
        let first = tokio::time::timeout(PATIENCE, client.next())
            .await
            .expect("a response in time")
            .expect("open")
            .expect("no error");
        assert!(matches!(
            first.response,
            Some(subscribe_response::Response::Update(_))
        ));
        let body = targets_until(&state, |body| {
            body["targets"].as_array().is_some_and(|targets| {
                targets.iter().any(|t| {
                    t["listen"] == plain_listen.as_str()
                        && !t["subscriber_list"][0]["last_drained"].is_null()
                })
            })
        })
        .await;
        let plain = body["targets"]
            .as_array()
            .expect("a targets array")
            .iter()
            .find(|t| t["listen"] == plain_listen.as_str())
            .expect("the plain target");
        assert_eq!(plain["subscribers"], 1, "{plain}");
        let listed = plain["subscriber_list"].as_array().expect("a list");
        assert_eq!(listed.len(), 1, "{plain}");
        let subscriber = &listed[0];
        let peer: SocketAddr = subscriber["peer"]
            .as_str()
            .expect("the client's address")
            .parse()
            .expect("an address");
        assert_eq!(peer.ip().to_string(), "127.0.0.1");
        assert_ne!(peer.to_string(), plain_listen);
        assert_eq!(subscriber["mode"], "STREAM/SAMPLE");
        assert_eq!(subscriber["encoding"], "PROTO");
        assert_eq!(subscriber["stalled"], false);
        for field in ["since", "last_drained"] {
            let text = subscriber[field].as_str().expect("a time");
            DateTime::parse_from_rfc3339(text).unwrap_or_else(|e| panic!("{field} {text}: {e}"));
            assert!(text.ends_with('Z'), "UTC: {text}");
        }

        drop(client);
        let body = targets_until(&state, |body| {
            body["targets"].as_array().is_some_and(|targets| {
                targets
                    .iter()
                    .all(|t| t["subscribers"] == 0 && t["subscriber_list"] == serde_json::json!([]))
            })
        })
        .await;
        for target in body["targets"].as_array().expect("a targets array") {
            assert_eq!(target["subscribers"], 0, "the client is gone: {target}");
            assert_eq!(target["subscriber_list"], serde_json::json!([]));
        }

        stop_all(&state).await;
        let body = targets_until(&state, |body| body["targets"] == serde_json::json!([])).await;
        assert_eq!(
            body["targets"],
            serde_json::json!([]),
            "stopped targets are gone"
        );
    }
}
