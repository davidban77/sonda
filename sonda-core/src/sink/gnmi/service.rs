//! The gRPC service a [`GnmiTarget`] serves: a hand-written router over
//! `http::Request` in place of generated server code.
//!
//! `Capabilities` answers with the target's static capabilities.
//! `Subscribe` serves STREAM subscriptions in SAMPLE mode (TARGET_DEFINED is
//! served as SAMPLE) and ONCE subscriptions, from the target's latest-value
//! table. Every other method answers `UNIMPLEMENTED`.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tonic::codec::Streaming;
use tonic::server::{Grpc, NamedService};
use tonic::Status;
use tower::Service;

use super::matcher::SubscriptionMatcher;
use super::target::{GnmiTarget, SeriesUpdate, TargetState};
use crate::encoder::gnmi::proto::{self, subscribe_request, subscription_list};
use crate::sink::grpc_codec::{PreEncoded, PreEncodedCodec, ProstCodec};

/// The gRPC service name of gNMI.
pub const SERVICE_NAME: &str = "gnmi.gNMI";

/// The gNMI version `Capabilities` reports.
pub const GNMI_VERSION: &str = "0.10.0";

/// Responses one subscription holds for its client before it waits for the
/// client to read.
pub const OUTBOUND_CAPACITY: usize = 64;

/// The HTTP/2 path of `gNMI.Capabilities`.
const CAPABILITIES_PATH: &str = "/gnmi.gNMI/Capabilities";

/// The HTTP/2 path of `gNMI.Subscribe`.
const SUBSCRIBE_PATH: &str = "/gnmi.gNMI/Subscribe";

/// The key of `SubscribeResponse.update` (field 1, length-delimited).
const UPDATE_KEY: u8 = (1 << 3) | 2;

/// `SubscribeResponse { sync_response: true }`: field 3, varint, 1.
const SYNC_RESPONSE: &[u8] = &[(3 << 3), 1];

/// A boxed future returning `Result<T, E>`.
type BoxFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// Routes HTTP/2 requests by path for one target.
///
/// The service holds its target weakly, so neither the listener nor an open
/// RPC keeps the target alive. A request that arrives after the target is
/// gone answers `UNAVAILABLE`.
#[derive(Clone)]
pub struct GnmiService {
    target: Weak<GnmiTarget>,
    /// Held while a subscription is checked against `max_subscribers` and
    /// counted, so concurrent subscriptions cannot both take the last place.
    admission: Arc<Mutex<()>>,
}

impl GnmiService {
    /// A service for `target`.
    pub(crate) fn new(target: Weak<GnmiTarget>) -> Self {
        Self {
            target,
            admission: Arc::new(Mutex::new(())),
        }
    }
}

/// Matches the service name; nothing routes on it.
impl NamedService for GnmiService {
    const NAME: &'static str = SERVICE_NAME;
}

impl Service<http::Request<tonic::body::Body>> for GnmiService {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let target = self.target.clone();
        let admission = Arc::clone(&self.admission);
        Box::pin(async move {
            if target.strong_count() == 0 {
                return Ok(target_stopped().into_http());
            }
            let response = match req.uri().path() {
                CAPABILITIES_PATH => {
                    Grpc::new(ProstCodec::<
                        proto::CapabilityResponse,
                        proto::CapabilityRequest,
                    >::default())
                    .unary(CapabilitiesHandler, req)
                    .await
                }
                SUBSCRIBE_PATH => {
                    Grpc::new(PreEncodedCodec::<proto::SubscribeRequest>::default())
                        .streaming(SubscribeHandler { target, admission }, req)
                        .await
                }
                path => {
                    Status::unimplemented(format!("{path} is not a method this gnmi target serves"))
                        .into_http()
                }
            };
            Ok(response)
        })
    }
}

/// `UNAVAILABLE`: the target this service belonged to has stopped.
fn target_stopped() -> Status {
    Status::unavailable("this gnmi target has stopped")
}

/// Answers `gNMI.Capabilities`.
#[derive(Clone)]
struct CapabilitiesHandler;

impl Service<tonic::Request<proto::CapabilityRequest>> for CapabilitiesHandler {
    type Response = tonic::Response<proto::CapabilityResponse>;
    type Error = Status;
    type Future = std::future::Ready<Result<Self::Response, Status>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: tonic::Request<proto::CapabilityRequest>) -> Self::Future {
        std::future::ready(Ok(tonic::Response::new(proto::CapabilityResponse {
            supported_models: Vec::new(),
            supported_encodings: vec![proto::Encoding::Proto as i32],
            gnmi_version: GNMI_VERSION.to_string(),
        })))
    }
}

/// The responses of one subscription, as tonic streams them to the client.
type SubscribeStream = Responses;

/// The responses of one subscription: what its producer sent, then the
/// status the producer ended the subscription with, if any.
struct Responses {
    rx: mpsc::Receiver<Result<PreEncoded, Status>>,
    ended: Option<oneshot::Receiver<Status>>,
}

impl tokio_stream::Stream for Responses {
    type Item = Result<PreEncoded, Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(response) = std::task::ready!(this.rx.poll_recv(cx)) {
            return Poll::Ready(Some(response));
        }
        // Every sender is gone, so the producer has returned, after sending
        // its ending if it had one.
        Poll::Ready(
            this.ended
                .take()
                .and_then(|mut ended| ended.try_recv().ok())
                .map(Err),
        )
    }
}

/// Answers `gNMI.Subscribe`: reads the subscription list, then streams
/// from a producer task until the subscription ends.
#[derive(Clone)]
struct SubscribeHandler {
    target: Weak<GnmiTarget>,
    admission: Arc<Mutex<()>>,
}

impl Service<tonic::Request<Streaming<proto::SubscribeRequest>>> for SubscribeHandler {
    type Response = tonic::Response<SubscribeStream>;
    type Error = Status;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: tonic::Request<Streaming<proto::SubscribeRequest>>) -> Self::Future {
        Box::pin(subscribe(
            self.target.clone(),
            Arc::clone(&self.admission),
            req.into_inner(),
        ))
    }
}

/// Start one subscription.
///
/// Reads the first request, which must carry a `SubscriptionList`; refuses
/// the list as [`SubscriptionMatcher::new`] does, and with
/// `RESOURCE_EXHAUSTED` when the target already serves `max_subscribers`
/// subscribers. Otherwise subscribes to the target's broadcast, which counts
/// the subscriber, spawns a task draining the rest of `inbound` and a
/// producer task, and returns the producer's response stream.
async fn subscribe(
    target: Weak<GnmiTarget>,
    admission: Arc<Mutex<()>>,
    mut inbound: Streaming<proto::SubscribeRequest>,
) -> Result<tonic::Response<SubscribeStream>, Status> {
    let list = match inbound.message().await? {
        Some(proto::SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(list)),
        }) => list,
        Some(_) => {
            return Err(Status::invalid_argument(
                "the first subscribe request must carry a subscription list",
            ))
        }
        None => {
            return Err(Status::invalid_argument(
                "the subscribe stream closed before its first request",
            ))
        }
    };
    let matcher = SubscriptionMatcher::new(&list)?;
    let once = list.mode == subscription_list::Mode::Once as i32;

    let held = target.upgrade().ok_or_else(target_stopped)?;
    let settings = held.settings();
    let samples = if once {
        Vec::new()
    } else {
        sample_groups(&list, settings.sample_floor)?
    };
    let updates = {
        let _admitted = admission.lock().unwrap_or_else(PoisonError::into_inner);
        if held.subscriber_count() >= settings.max_subscribers {
            return Err(Status::resource_exhausted(format!(
                "gnmi_target {} already serves its max_subscribers ({}) subscribers",
                held.local_addr(),
                settings.max_subscribers
            )));
        }
        held.subscribe()
    };
    let state = held.state();
    drop(held);

    let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
    let (ending, ended) = oneshot::channel();
    let (inbound_done, done) = oneshot::channel();
    // `inbound` is read outside any `select!`; the producer waits on `done`.
    let draining = tokio::spawn(async move {
        while let Ok(Some(_)) = inbound.message().await {}
        let _ = inbound_done.send(());
    });
    let producer = Producer {
        target,
        matcher,
        once,
        updates_only: list.updates_only,
        samples,
        updates,
        tx,
        ending,
    };
    tokio::spawn(producer.run(done, draining, state));
    Ok(tonic::Response::new(Responses {
        rx,
        ended: Some(ended),
    }))
}

/// The subscriptions of a STREAM list that share one sample period.
struct SampleGroup {
    period: Duration,
    matcher: SubscriptionMatcher,
    next: Instant,
}

/// One [`SampleGroup`] per distinct period in `list`, each due one period
/// from now. A subscription's period is its `sample_interval`, raised to
/// `floor`; an interval of 0 is the floor.
fn sample_groups(
    list: &proto::SubscriptionList,
    floor: Duration,
) -> Result<Vec<SampleGroup>, Status> {
    let mut periods: Vec<(Duration, Vec<proto::Subscription>)> = Vec::new();
    for subscription in &list.subscription {
        let period = Duration::from_nanos(subscription.sample_interval).max(floor);
        match periods.iter_mut().find(|(p, _)| *p == period) {
            Some((_, subscriptions)) => subscriptions.push(subscription.clone()),
            None => periods.push((period, vec![subscription.clone()])),
        }
    }
    let now = Instant::now();
    periods
        .into_iter()
        .map(|(period, subscription)| {
            let group = proto::SubscriptionList {
                subscription,
                ..list.clone()
            };
            Ok(SampleGroup {
                period,
                matcher: SubscriptionMatcher::new(&group)?,
                next: now + period,
            })
        })
        .collect()
}

/// `SubscribeResponse { update }` around one encoded `gnmi.Notification`.
fn update_response(notification: &Bytes) -> PreEncoded {
    let len = notification.len();
    let mut buf = BytesMut::with_capacity(1 + prost::length_delimiter_len(len) + len);
    buf.put_u8(UPDATE_KEY);
    prost::encoding::encode_varint(len as u64, &mut buf);
    buf.put_slice(notification);
    PreEncoded(buf.freeze())
}

/// `SubscribeResponse { sync_response: true }`.
fn sync_response() -> PreEncoded {
    PreEncoded(Bytes::from_static(SYNC_RESPONSE))
}

/// Resolves once the target's listener is no longer accepting connections,
/// which its last handle being dropped causes.
async fn listener_stopped(mut state: watch::Receiver<TargetState>) {
    let _ = state.wait_for(|s| *s != TargetState::Listening).await;
}

/// What one turn of [`Producer::run`] did.
enum Step {
    /// The client has gone, or (STREAM) its request stream ended.
    ClientDone,
    /// The target's listener stopped.
    TargetStopped,
    /// One response went to the client.
    Sent,
    /// A sample group is due.
    Sample,
}

/// The task that sends one subscription's responses.
struct Producer {
    target: Weak<GnmiTarget>,
    matcher: SubscriptionMatcher,
    once: bool,
    /// The initial sync sends only the sync marker.
    updates_only: bool,
    samples: Vec<SampleGroup>,
    /// Held so the subscription counts in the target's subscriber count.
    /// Drained at each sample; a lag is ignored, since every sample re-reads
    /// the table.
    updates: broadcast::Receiver<SeriesUpdate>,
    tx: mpsc::Sender<Result<PreEncoded, Status>>,
    /// Takes the status the subscription ends with when the target stopped.
    ending: oneshot::Sender<Status>,
}

impl Producer {
    /// Send the initial sync — the latest value of every matching series
    /// (none with `updates_only`), then the sync marker — and, for a STREAM
    /// list, every matching series again each time a sample group is due.
    ///
    /// Returns when the client stops receiving, after the sync marker for a
    /// ONCE list, for a STREAM list when the client's request stream has
    /// ended (`done`) and every queued response is sent, and when the
    /// target's listener stops or the target is gone, in which case the
    /// stream ends with `UNAVAILABLE` after the responses already sent.
    /// Returning drops the broadcast receiver, which uncounts the
    /// subscriber, ends the response stream and aborts `draining`.
    async fn run(
        mut self,
        mut done: oneshot::Receiver<()>,
        draining: JoinHandle<()>,
        state: watch::Receiver<TargetState>,
    ) {
        let stopped = listener_stopped(state);
        tokio::pin!(stopped);
        let ending = match self.initial_sync() {
            None => Some(target_stopped()),
            Some(mut pending) => loop {
                if self.once && pending.is_empty() {
                    break None;
                }
                let next_sample = self.samples.iter().map(|g| g.next).min();
                let step = tokio::select! {
                    _ = &mut stopped => Step::TargetStopped,
                    _ = &mut done, if !self.once && pending.is_empty() => Step::ClientDone,
                    _ = self.tx.closed() => Step::ClientDone,
                    permit = self.tx.reserve(), if !pending.is_empty() => match permit {
                        Ok(permit) => {
                            if let Some(response) = pending.pop_front() {
                                permit.send(Ok(response));
                            }
                            Step::Sent
                        }
                        Err(_) => Step::ClientDone,
                    },
                    _ = tokio::time::sleep_until(next_sample.unwrap_or_else(Instant::now)),
                        if pending.is_empty() && next_sample.is_some() => Step::Sample,
                };
                match step {
                    Step::ClientDone => break None,
                    Step::TargetStopped => break Some(target_stopped()),
                    Step::Sent => {}
                    Step::Sample => match self.sample() {
                        Some(responses) => pending = responses,
                        None => break Some(target_stopped()),
                    },
                }
            },
        };
        draining.abort();
        if let Some(status) = ending {
            let _ = self.ending.send(status);
        }
    }

    /// The latest value of every matching series (none with
    /// `updates_only`), then the sync marker. `None` once the target is
    /// gone.
    fn initial_sync(&self) -> Option<VecDeque<PreEncoded>> {
        let target = self.target.upgrade()?;
        let mut responses: VecDeque<PreEncoded> = if self.updates_only {
            VecDeque::new()
        } else {
            target
                .snapshot()
                .iter()
                .filter(|series| self.matcher.matches(&series.path))
                .map(|series| update_response(&series.latest))
                .collect()
        };
        drop(target);
        responses.push_back(sync_response());
        Some(responses)
    }

    /// The latest value of every series a due sample group matches, with
    /// each due group moved to its next period. `None` once the target is
    /// gone.
    fn sample(&mut self) -> Option<VecDeque<PreEncoded>> {
        loop {
            match self.updates.try_recv() {
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return None,
            }
        }
        let now = Instant::now();
        let due: Vec<&SubscriptionMatcher> = self
            .samples
            .iter()
            .filter(|g| g.next <= now)
            .map(|g| &g.matcher)
            .collect();
        let target = self.target.upgrade()?;
        let responses = target
            .snapshot()
            .iter()
            .filter(|series| due.iter().any(|m| m.matches(&series.path)))
            .map(|series| update_response(&series.latest))
            .collect();
        drop(target);
        for group in &mut self.samples {
            if group.next <= now {
                group.next = next_due(group.next, group.period, now);
            }
        }
        Some(responses)
    }
}

/// The first instant after `now` that is a whole number of `period`s after
/// `due`, which is at or before `now`; computed in one step however many
/// periods were missed.
fn next_due(due: Instant, period: Duration, now: Instant) -> Instant {
    let missed = now.saturating_duration_since(due).as_nanos() / period.as_nanos() + 1;
    let step = u64::try_from(period.as_nanos().saturating_mul(missed)).unwrap_or(u64::MAX);
    due + Duration::from_nanos(step)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::RwLock;

    use prost::Message;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_util::sync::CancellationToken;
    use tonic::transport::Channel;

    use super::*;
    use crate::config::{BaseScheduleConfig, ScenarioConfig};
    use crate::encoder::gnmi::path::parse_client_path;
    use crate::encoder::gnmi::proto::{subscribe_response, SubscriptionMode};
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
    use crate::encoder::{Encoder, EncoderConfig};
    use crate::generator::GeneratorConfig;
    use crate::model::metric::{Labels, MetricEvent};
    use crate::schedule::stats::ScenarioStats;
    use crate::sink::gnmi::target::DRAIN_GRACE;
    use crate::sink::gnmi::{registry, GnmiTargetConfig, GnmiTargetSink};
    use crate::sink::{create_sink, Sink, SinkConfig};
    use crate::{OnSinkError, SondaError};

    /// The template every test encoder renders.
    const TEMPLATE: &str = "/interfaces/interface[name={ifName}]/state/counters/{name}";

    /// How long a test waits for one response before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

    fn encoder_config() -> GnmiEncoderConfig {
        GnmiEncoderConfig {
            path: Some(TEMPLATE.to_string()),
            ..Default::default()
        }
    }

    /// A sink on its own ephemeral port, with `max_subscribers` and
    /// `sample_floor` when given.
    async fn sink(max_subscribers: Option<usize>, sample_floor: Option<&str>) -> GnmiTargetSink {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        cfg.max_subscribers = max_subscribers;
        cfg.sample_floor = sample_floor.map(str::to_string);
        GnmiTargetSink::new(&cfg)
            .await
            .expect("bind an ephemeral port")
    }

    /// Publish `name` for interface `if_name` through `sink`.
    async fn publish(sink: &mut GnmiTargetSink, name: &str, if_name: &str, value: f64) {
        let encoder = GnmiEncoder::new(&encoder_config()).expect("valid encoder");
        let event = MetricEvent::new(
            name.to_string(),
            value,
            Labels::from_pairs(&[("ifName", if_name)]).expect("valid labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        sink.write_metric_event(&event, &buf)
            .await
            .expect("published");
    }

    /// A STREAM list of one SAMPLE subscription to `path`, every
    /// `sample_interval`.
    fn stream(path: &str, sample_interval: Duration) -> proto::SubscriptionList {
        proto::SubscriptionList {
            prefix: None,
            subscription: vec![proto::Subscription {
                path: Some(parse_client_path(path).expect("a valid path")),
                mode: SubscriptionMode::Sample as i32,
                sample_interval: sample_interval.as_nanos() as u64,
                ..Default::default()
            }],
            mode: subscription_list::Mode::Stream as i32,
            encoding: proto::Encoding::Proto as i32,
            updates_only: false,
        }
    }

    /// A ONCE list for `path`.
    fn once(path: &str) -> proto::SubscriptionList {
        proto::SubscriptionList {
            mode: subscription_list::Mode::Once as i32,
            ..stream(path, Duration::ZERO)
        }
    }

    async fn connect(addr: SocketAddr) -> Channel {
        tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("a valid uri")
            .connect()
            .await
            .expect("connect to the target")
    }

    /// One open subscription. Dropping it closes the request stream and the
    /// connection.
    struct Client {
        responses: Streaming<proto::SubscribeResponse>,
        /// Holds the request stream open; `None` when it ended after the
        /// list.
        _requests: Option<mpsc::Sender<proto::SubscribeRequest>>,
        _channel: Channel,
    }

    /// Open a subscription with `list` on a connection of its own; the
    /// request stream stays open until the client is dropped.
    async fn open(addr: SocketAddr, list: proto::SubscriptionList) -> Result<Client, Status> {
        open_on(connect(addr).await, list, true).await
    }

    /// Open a subscription whose request stream ends right after the list,
    /// as a client sending a fixed sequence of requests does.
    async fn open_half_closed(
        addr: SocketAddr,
        list: proto::SubscriptionList,
    ) -> Result<Client, Status> {
        open_on(connect(addr).await, list, false).await
    }

    /// Open a subscription with `list` on `channel`; the request stream
    /// stays open while the client lives when `keep_open`, and ends after
    /// the list otherwise.
    async fn open_on(
        channel: Channel,
        list: proto::SubscriptionList,
        keep_open: bool,
    ) -> Result<Client, Status> {
        let (requests, outbound) = mpsc::channel(4);
        requests
            .send(proto::SubscribeRequest {
                request: Some(subscribe_request::Request::Subscribe(list)),
            })
            .await
            .expect("queue the subscription");
        let requests = keep_open.then_some(requests);
        let mut grpc = tonic::client::Grpc::new(channel.clone());
        grpc.ready().await.expect("the channel is ready");
        let responses = grpc
            .streaming(
                tonic::Request::new(ReceiverStream::new(outbound)),
                http::uri::PathAndQuery::from_static(SUBSCRIBE_PATH),
                ProstCodec::<proto::SubscribeRequest, proto::SubscribeResponse>::default(),
            )
            .await?
            .into_inner();
        Ok(Client {
            responses,
            _requests: requests,
            _channel: channel,
        })
    }

    /// One response as the tests read it.
    #[derive(Debug, PartialEq)]
    enum Received {
        /// An update for the series at this path, in gNMI string form.
        Update(String),
        Sync,
        /// The stream ended with `OK`.
        End,
    }

    async fn next(client: &mut Client) -> Received {
        let message = tokio::time::timeout(PATIENCE, client.responses.message())
            .await
            .expect("a response in time")
            .expect("no error on the stream");
        match message.and_then(|m| m.response) {
            None => Received::End,
            Some(subscribe_response::Response::SyncResponse(true)) => Received::Sync,
            Some(subscribe_response::Response::SyncResponse(false)) => {
                panic!("sync_response: false is never sent")
            }
            Some(subscribe_response::Response::Update(notification)) => {
                Received::Update(series_path(&notification))
            }
        }
    }

    /// The full path of a notification's first update, as the target keys it.
    fn series_path(notification: &proto::Notification) -> String {
        let prefix = notification.prefix.clone().unwrap_or_default();
        let elem = notification
            .update
            .first()
            .and_then(|u| u.path.clone())
            .expect("an update with a path")
            .elem;
        proto::Path {
            origin: prefix.origin,
            elem,
            target: prefix.target,
        }
        .to_string()
    }

    /// Updates per series path received until `deadline`.
    async fn count_until(client: &mut Client, deadline: Instant) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        while let Ok(message) = tokio::time::timeout_at(deadline, client.responses.message()).await
        {
            let response = message.expect("no error on the stream").expect("open");
            if let Some(subscribe_response::Response::Update(n)) = response.response {
                *counts.entry(series_path(&n)).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Wait until `done` holds, polling; false if it never does within
    /// [`PATIENCE`].
    async fn eventually(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        done()
    }

    fn counter(if_name: &str, leaf: &str) -> String {
        format!("openconfig:/interfaces/interface[name={if_name}]/state/counters/{leaf}")
    }

    #[test]
    fn update_response_decodes_as_a_subscribe_response_update() {
        let notification = proto::Notification {
            timestamp: 7,
            ..Default::default()
        };
        let framed = update_response(&Bytes::from(notification.encode_to_vec()));
        let decoded = proto::SubscribeResponse::decode(framed.0).expect("a subscribe response");
        assert_eq!(
            decoded.response,
            Some(subscribe_response::Response::Update(notification))
        );

        let sync = proto::SubscribeResponse::decode(sync_response().0).expect("decodes");
        assert_eq!(
            sync.response,
            Some(subscribe_response::Response::SyncResponse(true))
        );
    }

    #[tokio::test]
    async fn capabilities_reports_proto_and_version() {
        let sink = sink(None, None).await;
        let mut grpc = tonic::client::Grpc::new(connect(sink.target().local_addr()).await);
        grpc.ready().await.expect("the channel is ready");
        let capabilities = grpc
            .unary(
                tonic::Request::new(proto::CapabilityRequest {}),
                http::uri::PathAndQuery::from_static(CAPABILITIES_PATH),
                ProstCodec::<proto::CapabilityRequest, proto::CapabilityResponse>::default(),
            )
            .await
            .expect("capabilities")
            .into_inner();
        assert_eq!(capabilities.gnmi_version, "0.10.0");
        assert_eq!(
            capabilities.supported_encodings,
            vec![proto::Encoding::Proto as i32]
        );
        assert!(capabilities.supported_models.is_empty());
    }

    #[tokio::test]
    async fn other_methods_are_unimplemented() {
        let sink = sink(None, None).await;
        let mut grpc = tonic::client::Grpc::new(connect(sink.target().local_addr()).await);
        grpc.ready().await.expect("the channel is ready");
        let status = grpc
            .unary(
                tonic::Request::new(proto::CapabilityRequest {}),
                http::uri::PathAndQuery::from_static("/gnmi.gNMI/Get"),
                ProstCodec::<proto::CapabilityRequest, proto::CapabilityResponse>::default(),
            )
            .await
            .expect_err("Get is not served");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("/gnmi.gNMI/Get"), "{status}");
    }

    /// A request reaching a service whose target is gone is `UNAVAILABLE`.
    #[tokio::test]
    async fn requests_after_the_target_is_gone_are_unavailable() {
        let mut service = GnmiService::new(Weak::new());
        let request = http::Request::builder()
            .uri(SUBSCRIBE_PATH)
            .body(tonic::body::Body::empty())
            .expect("a request");
        let response = service.call(request).await.expect("infallible");
        let status = Status::from_header_map(response.headers()).expect("a grpc status");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn once_returns_matching_series_then_sync_then_closes() {
        let mut sink = sink(None, None).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "in_octets", "eth1", 2.0).await;
        publish(&mut sink, "out_octets", "eth0", 3.0).await;
        assert_eq!(sink.target().series_count(), 3);

        let mut client = open(
            sink.target().local_addr(),
            once("/interfaces/interface[name=*]/state/counters/in-octets"),
        )
        .await
        .expect("subscribed");

        let mut updates = vec![next(&mut client).await, next(&mut client).await];
        updates.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(
            updates,
            [
                Received::Update(counter("eth0", "in-octets")),
                Received::Update(counter("eth1", "in-octets")),
            ]
        );
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(next(&mut client).await, Received::End);
    }

    /// The initial sync of a STREAM subscription is the same as ONCE's, and
    /// the stream stays open after the sync marker.
    #[tokio::test]
    async fn stream_sends_the_initial_sync_then_samples() {
        let mut sink = sink(None, Some("50ms")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "out_octets", "eth0", 2.0).await;
        assert_eq!(sink.target().series_count(), 2);

        let mut client = open(
            sink.target().local_addr(),
            stream(
                "/interfaces/interface/state/counters/in-octets",
                Duration::ZERO,
            ),
        )
        .await
        .expect("subscribed");
        let expected = Received::Update(counter("eth0", "in-octets"));
        assert_eq!(next(&mut client).await, expected);
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(next(&mut client).await, expected, "the first sample");
    }

    /// A real scenario at 10 Hz publishes into the target; a subscriber
    /// sampling every second receives about one update per series per
    /// second, not ten.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sample_cadence_is_the_subscribers_not_the_schedulers() {
        let held = registry()
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = held.local_addr();
        let cancel = CancellationToken::new();
        let mut scenarios = Vec::new();
        for (name, if_name) in [
            ("in_octets", "eth0"),
            ("in_octets", "eth1"),
            ("out_octets", "eth0"),
        ] {
            let sink_config = SinkConfig::GnmiTarget(GnmiTargetConfig::new(addr.to_string()));
            let mut sink = create_sink(&sink_config, None)
                .await
                .expect("join the target");
            let config = scenario(name, if_name, 10.0, sink_config);
            let stats = Arc::new(RwLock::new(ScenarioStats::default()));
            let cancel = cancel.clone();
            let task_stats = Arc::clone(&stats);
            scenarios.push((
                stats,
                tokio::spawn(async move {
                    crate::schedule::runner::run_with_sink(
                        &config,
                        &mut sink,
                        &cancel,
                        Some(task_stats),
                    )
                    .await
                }),
            ));
        }
        assert!(eventually(|| held.series_count() == 3).await);

        let mut client = open(
            addr,
            stream(
                "/interfaces/interface[name=*]/state/counters/in-octets",
                Duration::from_secs(1),
            ),
        )
        .await
        .expect("subscribed");
        let counts = count_until(&mut client, Instant::now() + Duration::from_secs(5)).await;
        let published: Vec<u64> = scenarios
            .iter()
            .map(|(stats, _)| stats.read().unwrap().total_events)
            .collect();
        cancel.cancel();
        for (_, task) in scenarios {
            task.await.expect("no panic").expect("the scenario ran");
        }

        assert_eq!(counts.len(), 2, "both matching series arrived: {counts:?}");
        for if_name in ["eth0", "eth1"] {
            let n = counts[&counter(if_name, "in-octets")];
            assert!((4..=6).contains(&n), "{if_name}: {n} updates in 5 s");
        }
        assert!(
            published.iter().all(|&n| n >= 40),
            "each scenario published at 10 Hz: {published:?}"
        );
    }

    #[tokio::test]
    async fn sample_floor_clamps_short_intervals() {
        let mut sink = sink(None, Some("500ms")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "in_octets", "eth1", 2.0).await;
        assert_eq!(sink.target().series_count(), 2);

        let mut client = open(
            sink.target().local_addr(),
            stream(
                "/interfaces/interface/state/counters",
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("subscribed");
        let counts = count_until(&mut client, Instant::now() + Duration::from_secs(1)).await;
        assert_eq!(counts.len(), 2, "both series arrived: {counts:?}");
        for (series, n) in counts {
            assert!(n >= 2, "{series}: sampled at least once after the sync");
            assert!(n <= 3, "{series}: {n} updates in 1 s with a 500 ms floor");
        }
    }

    /// Records when each write reached the sink it wraps.
    struct Timed {
        inner: Box<dyn Sink>,
        at: Arc<Mutex<Vec<std::time::Instant>>>,
    }

    impl Timed {
        fn stamp(&self) {
            self.at.lock().unwrap().push(std::time::Instant::now());
        }
    }

    #[async_trait::async_trait]
    impl Sink for Timed {
        async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
            self.stamp();
            self.inner.write(data).await
        }

        async fn flush(&mut self) -> Result<(), SondaError> {
            self.inner.flush().await
        }

        fn wants_metric_events(&self) -> bool {
            self.inner.wants_metric_events()
        }

        async fn write_metric_event(
            &mut self,
            event: &MetricEvent,
            encoded: &[u8],
        ) -> Result<(), SondaError> {
            self.stamp();
            self.inner.write_metric_event(event, encoded).await
        }
    }

    /// The scheduled rate of [`publisher_run`], in Hz.
    const PUBLISHER_RATE: f64 = 1_000.0;

    /// How long [`publisher_run`] runs.
    const PUBLISHER_RUN: Duration = Duration::from_secs(3);

    /// The end of a run in which a tick owed but not yet emitted counts as
    /// late rather than dropped.
    const END_ALLOWANCE: Duration = Duration::from_millis(10);

    /// Ticks owed but not emitted, and the 99th percentile of how far the
    /// interval between consecutive writes strayed from the scheduled one.
    ///
    /// The loop never skips a tick while it runs: behind schedule it emits
    /// at once until it catches up. A tick is dropped only when the run ends
    /// before it is emitted, so ticks whose slot falls in the run's last
    /// [`END_ALLOWANCE`] are not counted as owed: a wake that lands up to
    /// that late is drift, which `p99_drift` measures, and with the timer's
    /// 1 ms granularity it happens to a sink that keeps up, the memory
    /// baseline included.
    struct PublisherRun {
        dropped: i64,
        p99_drift: Duration,
    }

    /// Run a [`PUBLISHER_RATE`] scenario for [`PUBLISHER_RUN`] into `sink`.
    async fn publisher_run(sink: Box<dyn Sink>, sink_config: SinkConfig) -> PublisherRun {
        let at = Arc::new(Mutex::new(Vec::new()));
        let mut sink: Box<dyn Sink> = Box::new(Timed {
            inner: sink,
            at: Arc::clone(&at),
        });
        let mut config = scenario("in_octets", "eth0", PUBLISHER_RATE, sink_config);
        config.base.duration = Some(format!("{}ms", PUBLISHER_RUN.as_millis()));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");

        let owed = (PUBLISHER_RATE * (PUBLISHER_RUN - END_ALLOWANCE).as_secs_f64()) as i64;
        let emitted = stats.read().unwrap().total_events as i64;
        let at = at.lock().unwrap();
        assert_eq!(at.len() as i64, emitted, "every tick reached the sink");
        let scheduled = Duration::from_secs_f64(1.0 / PUBLISHER_RATE);
        // The first tenth is warm-up.
        let mut drift: Vec<Duration> = at[at.len() / 10..]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs_diff(scheduled))
            .collect();
        drift.sort();
        PublisherRun {
            dropped: (owed - emitted).max(0),
            p99_drift: drift[drift.len() * 99 / 100],
        }
    }

    /// A publisher into a target with 64 subscribers that never read keeps
    /// its rate, and its timing stays within twice that of a publisher into
    /// a memory sink running at the same time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sixty_four_idle_subscribers_do_not_slow_the_publisher() {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        cfg.max_subscribers = Some(64);
        cfg.sample_floor = Some("100ms".to_string());
        let held = registry()
            .acquire(&cfg)
            .await
            .expect("bind an ephemeral port");
        cfg.listen = held.local_addr().to_string();
        let sink_config = SinkConfig::GnmiTarget(cfg);
        let sink = create_sink(&sink_config, None)
            .await
            .expect("join the target");
        // One connection each: a client library may close a connection
        // that buffers many unread small frames.
        let mut idle = Vec::new();
        for _ in 0..64 {
            let list = stream("/interfaces", Duration::from_millis(100));
            idle.push(open(held.local_addr(), list).await.expect("subscribed"));
        }
        assert_eq!(held.subscriber_count(), 64);

        let memory_config = SinkConfig::Memory {
            capture: false,
            max_events: None,
            capture_handle: None,
        };
        let memory = create_sink(&memory_config, None)
            .await
            .expect("a memory sink");
        let (target, baseline) = tokio::join!(
            publisher_run(sink, sink_config),
            publisher_run(memory, memory_config)
        );
        assert_eq!(held.subscriber_count(), 64, "every subscriber stayed open");
        drop(idle);

        eprintln!(
            "dropped ticks: memory {}, gnmi_target {}; p99 drift: memory {:?}, \
             gnmi_target with 64 idle subscribers {:?}",
            baseline.dropped, target.dropped, baseline.p99_drift, target.p99_drift
        );
        assert_eq!(target.dropped, 0, "the publisher kept its rate");
        assert!(
            target.p99_drift <= baseline.p99_drift * 2,
            "p99 drift {:?} with 64 idle subscribers, {:?} into memory",
            target.p99_drift,
            baseline.p99_drift
        );
    }

    #[tokio::test]
    async fn max_subscribers_refuses_with_resource_exhausted() {
        let mut sink = sink(Some(2), None).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        let addr = sink.target().local_addr();
        let list = || stream("/interfaces", Duration::ZERO);
        let _first = open(addr, list()).await.expect("first subscriber");
        let _second = open(addr, list()).await.expect("second subscriber");
        assert_eq!(sink.target().subscriber_count(), 2);

        let status = match open(addr, list()).await {
            Ok(_) => panic!("a third subscriber is admitted"),
            Err(status) => status,
        };
        assert_eq!(status.code(), tonic::Code::ResourceExhausted, "{status}");
        assert!(status.message().contains("max_subscribers"), "{status}");
        assert_eq!(sink.target().subscriber_count(), 2);
    }

    #[tokio::test]
    async fn client_disconnect_decrements_subscriber_count() {
        // No sample falls due while the test waits, so only the client
        // going away can end the subscription.
        let mut sink = sink(None, Some("10s")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        let mut client = open(
            sink.target().local_addr(),
            stream("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_eq!(
            next(&mut client).await,
            Received::Update(counter("eth0", "in-octets"))
        );
        assert_eq!(sink.target().subscriber_count(), 1);

        drop(client);
        let target = Arc::clone(sink.target());
        assert!(
            eventually(|| target.subscriber_count() == 0).await,
            "still {} subscribers",
            target.subscriber_count()
        );
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::poll(subscription_list::Mode::Poll, SubscriptionMode::Sample, "POLL")]
    #[case::on_change(subscription_list::Mode::Stream, SubscriptionMode::OnChange, "ON_CHANGE")]
    #[tokio::test]
    async fn unsupported_mode_is_unimplemented_with_mode_named(
        #[case] list_mode: subscription_list::Mode,
        #[case] mode: SubscriptionMode,
        #[case] named: &str,
    ) {
        let sink = sink(None, None).await;
        let mut list = stream("/interfaces", Duration::ZERO);
        list.mode = list_mode as i32;
        list.subscription[0].mode = mode as i32;
        let status = match open(sink.target().local_addr(), list).await {
            Ok(_) => panic!("{named} is served"),
            Err(status) => status,
        };
        assert_eq!(status.code(), tonic::Code::Unimplemented, "{status}");
        assert!(status.message().contains(named), "{status}");
        assert_eq!(sink.target().subscriber_count(), 0, "nothing was counted");
    }

    #[tokio::test]
    async fn the_first_request_must_carry_a_subscription_list() {
        let sink = sink(None, None).await;
        let channel = connect(sink.target().local_addr()).await;
        let (requests, outbound) = mpsc::channel(1);
        requests
            .send(proto::SubscribeRequest {
                request: Some(subscribe_request::Request::Poll(proto::Poll {})),
            })
            .await
            .unwrap();
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.unwrap();
        let status = grpc
            .streaming(
                tonic::Request::new(ReceiverStream::new(outbound)),
                http::uri::PathAndQuery::from_static(SUBSCRIBE_PATH),
                ProstCodec::<proto::SubscribeRequest, proto::SubscribeResponse>::default(),
            )
            .await
            .expect_err("a poll is not a subscription");
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
    }

    /// Dropping the last sink ends an open STREAM subscription with
    /// `UNAVAILABLE`, and the target's listener, within the teardown bound.
    /// The stream ends before the target would close the connection, so the
    /// subscription ended itself.
    #[tokio::test]
    async fn teardown_with_open_stream_is_bounded() {
        // No sample falls due during the test.
        let mut sink = sink(None, Some("10s")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        let mut client = open(
            sink.target().local_addr(),
            stream("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_eq!(
            next(&mut client).await,
            Received::Update(counter("eth0", "in-octets")),
            "the stream delivered before the drop"
        );

        let stopped = sink.target().wait();
        let started = Instant::now();
        drop(sink);
        let ending = loop {
            match tokio::time::timeout_at(
                started + Duration::from_secs(2),
                client.responses.message(),
            )
            .await
            .expect("the stream ends within 2 s")
            {
                Ok(Some(_)) => continue,
                ending => break ending,
            }
        };
        assert!(
            started.elapsed() < DRAIN_GRACE,
            "the stream ended with {ending:?} after {:?}",
            started.elapsed()
        );
        let status = ending.expect_err("the stream ends with a status, not OK");
        assert_eq!(status.code(), tonic::Code::Unavailable, "{status}");
        tokio::time::timeout_at(started + Duration::from_secs(2), stopped)
            .await
            .expect("the target stops within 2 s");
    }

    /// A ONCE subscription cut off by the target stopping ends with
    /// `UNAVAILABLE` after the updates already sent, not with `OK`.
    #[tokio::test]
    async fn a_once_cut_off_by_the_target_stopping_ends_unavailable() {
        const SERIES: usize = 2_000;
        let mut sink = sink(None, None).await;
        for i in 0..SERIES {
            publish(&mut sink, "in_octets", &format!("eth{i}"), 1.0).await;
        }
        assert_eq!(sink.target().series_count(), SERIES);
        // A 64 KiB window holds far fewer than SERIES updates, so the
        // subscription is still sending when the target stops.
        let channel = tonic::transport::Endpoint::from_shared(format!(
            "http://{}",
            sink.target().local_addr()
        ))
        .expect("a valid uri")
        .initial_stream_window_size(65_535)
        .initial_connection_window_size(65_535)
        .connect()
        .await
        .expect("connect to the target");
        let mut client = open_on(channel, once("/interfaces"), true)
            .await
            .expect("subscribed");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(sink);

        let mut updates = 0;
        let ending = loop {
            match tokio::time::timeout(PATIENCE, client.responses.message())
                .await
                .expect("a response in time")
            {
                Ok(Some(proto::SubscribeResponse {
                    response: Some(subscribe_response::Response::Update(_)),
                })) => updates += 1,
                Ok(Some(other)) => panic!("after {updates} updates: {other:?}"),
                ending => break ending,
            }
        };
        assert!(updates > 0, "the subscription sent before the stop");
        assert!(updates < SERIES, "the stop cut the sync short");
        let status = ending.expect_err("the stream ends with a status, not OK");
        assert_eq!(status.code(), tonic::Code::Unavailable, "{status}");
    }

    /// A client that half-closes its request stream right after the list
    /// still receives every matching series, then the sync marker.
    #[tokio::test]
    async fn a_half_closed_request_stream_still_receives_the_full_sync() {
        const SERIES: usize = 20;
        let mut sink = sink(None, Some("10s")).await;
        for i in 0..SERIES {
            publish(&mut sink, "in_octets", &format!("eth{i}"), 1.0).await;
        }
        assert_eq!(sink.target().series_count(), SERIES);
        let addr = sink.target().local_addr();
        for _ in 0..5 {
            let mut client = open_half_closed(addr, stream("/interfaces", Duration::ZERO))
                .await
                .expect("subscribed");
            for i in 0..SERIES {
                assert!(
                    matches!(next(&mut client).await, Received::Update(_)),
                    "update {i} of {SERIES}"
                );
            }
            assert_eq!(next(&mut client).await, Received::Sync);
        }
    }

    /// With `updates_only` the initial sync is the sync marker alone: a
    /// STREAM subscription's first update is its first sample, and a ONCE
    /// subscription ends after the marker.
    #[tokio::test]
    async fn updates_only_sends_no_initial_state() {
        let mut sink = sink(None, Some("300ms")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        assert_eq!(sink.target().series_count(), 1);
        let addr = sink.target().local_addr();

        let mut list = stream("/interfaces", Duration::ZERO);
        list.updates_only = true;
        let started = Instant::now();
        let mut client = open(addr, list).await.expect("subscribed");
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(
            next(&mut client).await,
            Received::Update(counter("eth0", "in-octets"))
        );
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "the first update came after {:?}, before the first sample",
            started.elapsed()
        );

        let mut list = once("/interfaces");
        list.updates_only = true;
        let mut client = open(addr, list).await.expect("subscribed");
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(next(&mut client).await, Received::End);
    }

    #[test]
    fn next_due_skips_missed_periods_in_one_step() {
        let due = Instant::now();
        let period = Duration::from_micros(1);
        let now = due + Duration::from_secs(3_600);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(next_due(due, period, now));
        });
        let next = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("computed without walking every missed period");
        assert_eq!(next, now + period, "the first slot after now");

        let late = due + Duration::from_millis(2_500);
        assert_eq!(
            next_due(due, Duration::from_secs(1), late),
            due + Duration::from_secs(3)
        );
        assert_eq!(
            next_due(due, Duration::from_secs(1), due),
            due + Duration::from_secs(1),
            "due exactly now moves one period"
        );
    }

    /// A metrics scenario `name{ifName=if_name}` at `rate` Hz into `sink`.
    fn scenario(name: &str, if_name: &str, rate: f64, sink: SinkConfig) -> ScenarioConfig {
        ScenarioConfig {
            base: BaseScheduleConfig {
                gap_windows: None,
                name: name.to_string(),
                rate,
                duration: None,
                gaps: None,
                bursts: None,
                cardinality_spikes: None,
                dynamic_labels: None,
                labels: Some(HashMap::from([("ifName".to_string(), if_name.to_string())])),
                sink,
                phase_offset: None,
                clock_group: None,
                clock_group_is_auto: None,
                start_time: None,
                jitter: None,
                jitter_seed: None,
                on_sink_error: OnSinkError::Fail,
            },
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: EncoderConfig::Gnmi(encoder_config()),
            metric_type: None,
            help: None,
        }
    }
}
