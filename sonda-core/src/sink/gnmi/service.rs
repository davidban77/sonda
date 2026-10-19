//! The gRPC service a [`GnmiTarget`] serves: a hand-written router over
//! `http::Request` in place of generated server code.
//!
//! `Capabilities` answers with the target's static capabilities.
//! `Subscribe` serves STREAM subscriptions in SAMPLE, ON_CHANGE and
//! TARGET_DEFINED mode, and ONCE subscriptions, from the target's
//! latest-value table and its broadcast of changed values. Every other
//! method answers `UNIMPLEMENTED`.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Weak;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tonic::codec::Streaming;
use tonic::server::{Grpc, NamedService};
use tonic::Status;
use tower::Service;

use super::matcher::SubscriptionMatcher;
use super::notification::ValueKind;
use super::target::{
    delete_notification, GnmiTarget, PathKey, SeriesEvent, SeriesId, SeriesSnapshot, SeriesUpdate,
    TargetState,
};
use crate::encoder::gnmi::proto::{self, subscribe_request, subscription_list, SubscriptionMode};
use crate::sink::grpc_codec::{PreEncoded, PreEncodedCodec, ProstCodec};

/// The gRPC service name of gNMI.
pub const SERVICE_NAME: &str = "gnmi.gNMI";

/// The gNMI version `Capabilities` reports.
pub const GNMI_VERSION: &str = "0.10.0";

/// Responses one subscription holds for its client before it waits for the
/// client to read.
pub const OUTBOUND_CAPACITY: usize = 64;

/// The least time a subscription with responses waiting may go without its
/// client draining one before it is ended as stalled.
pub(crate) const STALL_FLOOR: Duration = Duration::from_secs(10);

/// Series an ON_CHANGE set's match cache holds, at least, before it is
/// pruned to the series in the table.
const MATCHED_PRUNE_FLOOR: usize = 1024;

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
}

impl GnmiService {
    /// A service for `target`.
    pub(crate) fn new(target: Weak<GnmiTarget>) -> Self {
        Self { target }
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
                        .streaming(SubscribeHandler { target }, req)
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
}

impl Service<tonic::Request<Streaming<proto::SubscribeRequest>>> for SubscribeHandler {
    type Response = tonic::Response<SubscribeStream>;
    type Error = Status;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: tonic::Request<Streaming<proto::SubscribeRequest>>) -> Self::Future {
        let peer = req.remote_addr();
        Box::pin(subscribe(self.target.clone(), peer, req.into_inner()))
    }
}

/// Start one subscription.
///
/// Reads the first request, which must carry a `SubscriptionList`, and
/// refuses or [starts](start) it. Spawns a task reading the rest of
/// `inbound` and the producer task, and returns the producer's response
/// stream.
async fn subscribe(
    target: Weak<GnmiTarget>,
    peer: Option<SocketAddr>,
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
    let (producer, responses) = start(target, peer, &list)?;
    // Further requests are read and ignored, outside any `select!`. The
    // request stream ending does not end the subscription.
    let draining = tokio::spawn(async move { while let Ok(Some(_)) = inbound.message().await {} });
    tokio::spawn(producer.run(draining));
    Ok(tonic::Response::new(responses))
}

/// The producer serving `list` and the response stream it feeds.
///
/// Refuses `list` as [`SubscriptionMatcher::new`] and [`plan_groups`] do,
/// and with `RESOURCE_EXHAUSTED` when the target already serves
/// `max_subscribers` subscribers. Otherwise takes one of the target's
/// subscriber places, which the producer holds, and, when the list has
/// ON_CHANGE subscriptions, subscribes to the target's broadcast before the
/// producer takes its snapshot.
fn start(
    target: Weak<GnmiTarget>,
    peer: Option<SocketAddr>,
    list: &proto::SubscriptionList,
) -> Result<(Producer, Responses), Status> {
    let matcher = SubscriptionMatcher::new(list)?;
    let once = list.mode == subscription_list::Mode::Once as i32;

    let held = target.upgrade().ok_or_else(target_stopped)?;
    let settings = held.settings();
    let plan = if once {
        Plan::default()
    } else {
        plan_groups(list, settings.sample_floor)?
    };
    let place = held.admit().ok_or_else(|| {
        Status::resource_exhausted(format!(
            "gnmi_target {} already serves its max_subscribers ({}) subscribers",
            held.local_addr(),
            settings.max_subscribers
        ))
    })?;
    let stall_limit = stall_limit(plan.samples.iter().map(|g| g.period), settings.stall_floor);
    // Subscribed before the initial sync takes its snapshot, so every
    // change the snapshot does not hold arrives on the receiver.
    let on_change = (!plan.on_change.is_empty()).then(|| OnChangeSet {
        selectors: plan.on_change,
        rx: held.subscribe(),
        matched: HashMap::new(),
        prune_at: MATCHED_PRUNE_FLOOR,
    });
    #[cfg(test)]
    let probe = std::sync::Arc::clone(&settings.probe);
    let state = held.state();
    drop(held);

    let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
    let (ending, ended) = oneshot::channel();
    let producer = Producer {
        target,
        matcher,
        once,
        updates_only: list.updates_only,
        samples: plan.samples,
        heartbeats: plan.heartbeats,
        on_change,
        stall_limit,
        peer,
        _place: place,
        state,
        tx,
        ending,
        #[cfg(test)]
        probe,
    };
    let responses = Responses {
        rx,
        ended: Some(ended),
    };
    Ok((producer, responses))
}

/// The kinds of value a group of subscriptions serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KindFilter {
    /// Every kind.
    Any,
    /// `string_val` only.
    Strings,
    /// Every kind but `string_val`.
    NonStrings,
}

impl KindFilter {
    /// Whether a series whose values are of `kind` is served.
    fn admits(self, kind: ValueKind) -> bool {
        match self {
            Self::Any => true,
            Self::Strings => kind == ValueKind::String,
            Self::NonStrings => kind != ValueKind::String,
        }
    }
}

/// Subscriptions of one list that are served alike, and the kinds of value
/// they serve.
struct Selector {
    matcher: SubscriptionMatcher,
    kinds: KindFilter,
}

impl Selector {
    /// Whether the series at `path`, whose values are of `kind`, is served.
    fn selects(&self, path: &PathKey, kind: ValueKind) -> bool {
        self.kinds.admits(kind) && self.matcher.matches(path.path())
    }
}

/// Subscriptions of a STREAM list served on one timer: sampled, or sent as
/// heartbeats.
struct Group {
    period: Duration,
    selector: Selector,
    next: Instant,
    /// For a SAMPLE group with `suppress_redundant`, what the group last
    /// sent of each series it selects; `None` for every other group.
    sent: Option<HashMap<SeriesId, Sent>>,
}

/// A series a suppressing sample group has sent.
struct Sent {
    /// The version of the value last sent.
    version: u64,
    /// The series' path, for its delete once it leaves the table.
    path: PathKey,
}

/// How a STREAM list is served.
#[derive(Default)]
struct Plan {
    samples: Vec<Group>,
    heartbeats: Vec<Group>,
    /// The selectors of the ON_CHANGE set; empty when the list has no
    /// ON_CHANGE or TARGET_DEFINED subscription.
    on_change: Vec<Selector>,
}

/// Split a STREAM `list` into sample groups, heartbeat groups and the
/// ON_CHANGE set:
///
/// - SAMPLE: a sample group at its `sample_interval`. With
///   `suppress_redundant`, the group sends a series only when its value
///   changed since the group last sent it, and a heartbeat group at its
///   `heartbeat_interval` re-sends every series it matches unless that is 0.
///   Without `suppress_redundant`, `heartbeat_interval` is not read: every
///   sample already sends every series.
/// - ON_CHANGE: the ON_CHANGE set, and a heartbeat group at its
///   `heartbeat_interval` unless that is 0.
/// - TARGET_DEFINED: for `string_val` series, the ON_CHANGE set and a
///   heartbeat group as for ON_CHANGE; for every other kind, a sample group
///   at its `sample_interval` that does not suppress.
///
/// Every interval is raised to `floor`, so a sample interval of 0 is the
/// floor. Subscriptions with the same period, kinds and suppression share a
/// group, due one period from now; heartbeat groups never suppress.
fn plan_groups(list: &proto::SubscriptionList, floor: Duration) -> Result<Plan, Status> {
    type Timed = Vec<((Duration, KindFilter, bool), Vec<proto::Subscription>)>;
    let (mut samples, mut heartbeats): (Timed, Timed) = (Vec::new(), Vec::new());
    let mut on_change: Vec<(KindFilter, Vec<proto::Subscription>)> = Vec::new();
    for subscription in &list.subscription {
        let sample = Duration::from_nanos(subscription.sample_interval).max(floor);
        let heartbeat = (subscription.heartbeat_interval > 0)
            .then(|| Duration::from_nanos(subscription.heartbeat_interval).max(floor));
        let mut changes = |kinds: KindFilter| {
            add(&mut on_change, kinds, subscription);
            if let Some(period) = heartbeat {
                add(&mut heartbeats, (period, kinds, false), subscription);
            }
        };
        // The matcher has refused every mode but these in a STREAM list.
        match SubscriptionMode::try_from(subscription.mode) {
            Ok(SubscriptionMode::OnChange) => changes(KindFilter::Any),
            Ok(SubscriptionMode::TargetDefined) => {
                changes(KindFilter::Strings);
                add(
                    &mut samples,
                    (sample, KindFilter::NonStrings, false),
                    subscription,
                );
            }
            _ => {
                let suppress = subscription.suppress_redundant;
                add(
                    &mut samples,
                    (sample, KindFilter::Any, suppress),
                    subscription,
                );
                if let Some(period) = heartbeat.filter(|_| suppress) {
                    add(
                        &mut heartbeats,
                        (period, KindFilter::Any, false),
                        subscription,
                    );
                }
            }
        }
    }
    let selector = |kinds: KindFilter, subscription: Vec<proto::Subscription>| {
        let group = proto::SubscriptionList {
            subscription,
            ..list.clone()
        };
        Ok::<_, Status>(Selector {
            matcher: SubscriptionMatcher::new(&group)?,
            kinds,
        })
    };
    let now = Instant::now();
    let groups = |timed: Timed| {
        timed
            .into_iter()
            .map(|((period, kinds, suppress), subscription)| {
                Ok(Group {
                    period,
                    selector: selector(kinds, subscription)?,
                    next: now + period,
                    sent: suppress.then(HashMap::new),
                })
            })
            .collect::<Result<Vec<_>, Status>>()
    };
    Ok(Plan {
        samples: groups(samples)?,
        heartbeats: groups(heartbeats)?,
        on_change: on_change
            .into_iter()
            .map(|(kinds, subscription)| selector(kinds, subscription))
            .collect::<Result<_, _>>()?,
    })
}

/// Add `subscription` to the entry of `groups` keyed `key`, adding the entry
/// when there is none.
fn add<K: PartialEq>(
    groups: &mut Vec<(K, Vec<proto::Subscription>)>,
    key: K,
    subscription: &proto::Subscription,
) {
    match groups.iter_mut().find(|(k, _)| *k == key) {
        Some((_, subscriptions)) => subscriptions.push(subscription.clone()),
        None => groups.push((key, vec![subscription.clone()])),
    }
}

/// The ON_CHANGE subscriptions of a list: the receiver of the target's
/// broadcast of changed values, which series they match, and which version
/// of each was last queued.
struct OnChangeSet {
    selectors: Vec<Selector>,
    rx: broadcast::Receiver<SeriesEvent>,
    /// What the set knows of each series seen so far.
    matched: HashMap<SeriesId, Seen>,
    /// The size of `matched` past which it is pruned.
    prune_at: usize,
}

/// What an [`OnChangeSet`] knows of one series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    /// Whether the series matches, decided when it is first seen.
    matches: bool,
    /// The version of the series' value last queued, by the initial sync, a
    /// resync, a heartbeat or a change; 0 before any.
    queued: u64,
}

impl OnChangeSet {
    /// What the set knows of series `id`, whose values are of `kind`.
    /// Whether it matches is decided the first time the series is seen,
    /// from the path `path` returns; a series without a path does not
    /// match.
    fn seen(
        &mut self,
        id: SeriesId,
        kind: ValueKind,
        path: impl FnOnce() -> Option<PathKey>,
    ) -> &mut Seen {
        let selectors = &self.selectors;
        self.matched.entry(id).or_insert_with(|| Seen {
            matches: path().is_some_and(|path| selectors.iter().any(|s| s.selects(&path, kind))),
            queued: 0,
        })
    }

    /// Forget the series that are not in `snapshot`. `matched` may then grow
    /// to twice what is left, and at least [`MATCHED_PRUNE_FLOOR`], before it
    /// is pruned again.
    fn prune(&mut self, snapshot: &[SeriesSnapshot]) {
        self.retain(snapshot, |_| false);
    }

    /// Forget the series that are not in `snapshot` and that the set does
    /// not match, as [`OnChangeSet::prune`] does. A matched series stays
    /// until its removal is read from the broadcast.
    fn prune_unmatched(&mut self, snapshot: &[SeriesSnapshot]) {
        self.retain(snapshot, |seen| seen.matches);
    }

    /// Keep the series in `snapshot` and those `keep` holds for, and reset
    /// `prune_at` from what is left.
    fn retain(&mut self, snapshot: &[SeriesSnapshot], keep: impl Fn(&Seen) -> bool) {
        let live: HashSet<SeriesId> = snapshot.iter().map(|s| s.series_id).collect();
        self.matched
            .retain(|id, seen| live.contains(id) || keep(seen));
        self.prune_at = self
            .matched
            .len()
            .saturating_mul(2)
            .max(MATCHED_PRUNE_FLOOR);
    }
}

/// The next message on the ON_CHANGE set's broadcast; never resolves when
/// there is no set.
async fn next_change(on_change: &mut Option<OnChangeSet>) -> Result<SeriesEvent, RecvError> {
    match on_change {
        Some(set) => set.rx.recv().await,
        None => std::future::pending().await,
    }
}

/// One entry of a subscription's queue.
enum Queued {
    /// A response encoded when it was queued.
    Response(PreEncoded),
    /// A series whose latest value [`Pending::values`] holds.
    Value(SeriesId),
}

/// The responses a subscription has yet to send, in order. A series has at
/// most one value queued: queuing another replaces it in place.
#[derive(Default)]
struct Pending {
    queue: VecDeque<Queued>,
    values: HashMap<SeriesId, Bytes>,
    /// The [`Queued::Response`] entries in `queue`.
    responses: usize,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The series values in the queue.
    #[cfg(test)]
    fn queued_values(&self) -> usize {
        self.values.len()
    }

    /// Queue `response` last.
    fn push_response(&mut self, response: PreEncoded) {
        self.responses += 1;
        self.queue.push_back(Queued::Response(response));
    }

    /// Queue `latest`, one encoded notification, as series `id`'s value: in
    /// place of the value already queued for it, or last.
    fn push_value(&mut self, id: SeriesId, latest: Bytes) {
        match self.values.entry(id) {
            Entry::Occupied(mut queued) => {
                queued.insert(latest);
            }
            Entry::Vacant(slot) => {
                slot.insert(latest);
                self.queue.push_back(Queued::Value(id));
            }
        }
    }

    /// Queue `delete`, one encoded notification deleting series `id`, last,
    /// in place of any value queued for the series.
    fn push_delete(&mut self, id: SeriesId, delete: &Bytes) {
        self.values.remove(&id);
        self.push_response(update_response(delete));
    }

    /// Take the first response, encoding a queued value as an update.
    fn pop(&mut self) -> Option<PreEncoded> {
        loop {
            match self.queue.pop_front()? {
                Queued::Response(response) => {
                    self.responses -= 1;
                    return Some(response);
                }
                Queued::Value(id) => {
                    if let Some(latest) = self.values.remove(&id) {
                        return Some(update_response(&latest));
                    }
                }
            }
        }
    }
}

/// How long a subscription with responses waiting may go without its client
/// draining one: three of its longest sample periods, and never less than
/// `floor`. A ONCE list, and a list with only ON_CHANGE subscriptions, has no
/// sample periods, so its limit is `floor`; heartbeat periods do not count.
///
/// The limit runs from when the client's receive window is spent. Until then
/// the client's HTTP/2 layer accepts data whether or not the application
/// reads it, so the server cannot see a reader that stopped;
/// `max_subscribers` caps how many such clients hold a place.
fn stall_limit(periods: impl Iterator<Item = Duration>, floor: Duration) -> Duration {
    periods
        .max()
        .unwrap_or_default()
        .saturating_mul(3)
        .max(floor)
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

/// What the subscriptions of one target did, read by tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Probe {
    /// The most series values any subscription has held queued at once.
    pub(crate) max_queued_values: std::sync::atomic::AtomicUsize,
    /// Resyncs after a subscription's broadcast receiver lagged.
    pub(crate) resyncs: std::sync::atomic::AtomicUsize,
    /// Match-cache prunes run on reading a change.
    pub(crate) prunes: std::sync::atomic::AtomicUsize,
    /// Sample groups that fell due.
    pub(crate) samples: std::sync::atomic::AtomicUsize,
    /// Run by every resync after it takes its new broadcast receiver and
    /// before it reads the snapshot.
    pub(crate) after_resubscribe: Hook,
    /// Run by every heartbeat before it reads the snapshot.
    pub(crate) before_heartbeat: Hook,
}

/// A step a test sets for a subscription to run; nothing until it is set.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hook(std::sync::Mutex<Option<Box<dyn FnMut() + Send>>>);

#[cfg(test)]
impl Hook {
    /// Run `step` each time the hook runs, in place of any earlier step.
    pub(crate) fn set(&self, step: impl FnMut() + Send + 'static) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(step));
    }

    fn run(&self) {
        if let Some(step) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            step();
        }
    }
}

#[cfg(test)]
impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hook")
    }
}

/// What one turn of [`Producer::run`] did.
enum Step {
    /// The client has gone.
    ClientDone,
    /// The target's listener stopped.
    TargetStopped,
    /// The client drained no response for the stall limit.
    Stalled,
    /// One response went to the client.
    Sent,
    /// A sample group is due.
    Sample,
    /// A heartbeat group is due.
    Heartbeat,
    /// The ON_CHANGE set's broadcast delivered a change, or reported a lag.
    Change(Result<SeriesEvent, RecvError>),
}

/// The task that sends one subscription's responses.
struct Producer {
    target: Weak<GnmiTarget>,
    matcher: SubscriptionMatcher,
    once: bool,
    /// The initial sync sends only the sync marker.
    updates_only: bool,
    samples: Vec<Group>,
    heartbeats: Vec<Group>,
    on_change: Option<OnChangeSet>,
    /// How long responses may wait without the client draining one.
    stall_limit: Duration,
    /// The client's address, for the log lines when it stalls or lags.
    peer: Option<SocketAddr>,
    /// The subscriber place the subscription holds until the producer
    /// returns.
    _place: OwnedSemaphorePermit,
    /// The target's listener state.
    state: watch::Receiver<TargetState>,
    tx: mpsc::Sender<Result<PreEncoded, Status>>,
    /// Takes the status the subscription ends with when the target stopped
    /// or the client stalled.
    ending: oneshot::Sender<Status>,
    #[cfg(test)]
    probe: std::sync::Arc<Probe>,
}

impl Producer {
    /// Send the initial sync — the latest value of every matching series
    /// (none with `updates_only`), then the sync marker — and, for a STREAM
    /// list: every series a sample group matches each time the group is
    /// due; every change to a series the ON_CHANGE set matches, including a
    /// series first published after the subscription started; and the
    /// latest value of every series a heartbeat group matches each time the
    /// group is due, changed or not.
    ///
    /// A series has at most one value waiting: a newer change or heartbeat
    /// replaces it in place. A change is queued only when its version is
    /// newer than that of the series' value last queued, so a change the
    /// initial sync, a resync or a heartbeat already holds is not sent
    /// again. A sample group with `suppress_redundant` sends a series only
    /// when its version is newer than the one the group last sent; the
    /// initial sync counts as sent. Sample groups fall due only while no
    /// sampled response or sync marker waits. When the broadcast reports
    /// that the subscription fell more than the target's
    /// `subscriber_buffer` changes behind, the latest value of every series
    /// the ON_CHANGE set matches is queued again, without a second sync
    /// marker.
    ///
    /// When a series leaves the table, the subscription sends one delete for
    /// its path if its ON_CHANGE set matched the series (on the broadcast's
    /// removal) or a suppressing sample group had sent it (at that group's
    /// next sample), whichever comes first. The delete goes last in the
    /// queue, in place of any value waiting for the series. A plain SAMPLE
    /// subscription sends no delete; the series stops appearing.
    ///
    /// Returns when the client stops receiving, after the sync marker for a
    /// ONCE list, and when the target's listener stops or the target is
    /// gone, in which case the stream ends with `UNAVAILABLE` after the
    /// responses already sent. Also returns, ending the stream with
    /// `UNAVAILABLE` and logging the client's address, when responses wait
    /// for `stall_limit` with none taken from the queue. Responses leave the
    /// queue only as the client grants HTTP/2 flow-control credit, so a
    /// client that grants some within the limit, however little, is not
    /// ended. The client's request stream ending does not end a
    /// subscription. Returning gives the subscriber place back, ends the
    /// response stream and aborts `draining`.
    async fn run(mut self, draining: JoinHandle<()>) {
        let stopped = listener_stopped(self.state.clone());
        tokio::pin!(stopped);
        let ending = match self.initial_sync() {
            None => Some(target_stopped()),
            Some(mut pending) => {
                // When the queue last became non-empty or last sent a response.
                let mut waiting_since = Instant::now();
                loop {
                    if self.once && pending.is_empty() {
                        break None;
                    }
                    let next_sample = (pending.responses == 0)
                        .then(|| self.samples.iter().map(|g| g.next).min())
                        .flatten();
                    let next_heartbeat = self.heartbeats.iter().map(|g| g.next).min();
                    let was_empty = pending.is_empty();
                    let step = tokio::select! {
                        _ = &mut stopped => Step::TargetStopped,
                        _ = self.tx.closed() => Step::ClientDone,
                        permit = tokio::time::timeout_at(
                            waiting_since + self.stall_limit,
                            self.tx.reserve(),
                        ), if !pending.is_empty() => match permit {
                            Ok(Ok(permit)) => {
                                if let Some(response) = pending.pop() {
                                    permit.send(Ok(response));
                                }
                                Step::Sent
                            }
                            Ok(Err(_)) => Step::ClientDone,
                            Err(_) => Step::Stalled,
                        },
                        update = next_change(&mut self.on_change),
                            if self.on_change.is_some() => Step::Change(update),
                        _ = tokio::time::sleep_until(next_sample.unwrap_or_else(Instant::now)),
                            if next_sample.is_some() => Step::Sample,
                        _ = tokio::time::sleep_until(next_heartbeat.unwrap_or_else(Instant::now)),
                            if next_heartbeat.is_some() => Step::Heartbeat,
                    };
                    let served = match step {
                        Step::ClientDone => break None,
                        Step::TargetStopped => break Some(target_stopped()),
                        Step::Stalled => {
                            tracing::warn!(
                                peer = %self.peer_name(),
                                limit = ?self.stall_limit,
                                "gnmi subscriber stalled; ending its subscription"
                            );
                            break Some(Status::unavailable(format!(
                                "subscriber stalled: no data drained for {:?}",
                                self.stall_limit
                            )));
                        }
                        Step::Sent => {
                            waiting_since = Instant::now();
                            true
                        }
                        Step::Sample => self.sample(&mut pending),
                        Step::Heartbeat => self.heartbeat(&mut pending),
                        Step::Change(Ok(SeriesEvent::Changed(update))) => {
                            self.offer(update, &mut pending)
                        }
                        Step::Change(Ok(SeriesEvent::Removed { series_id, bytes })) => {
                            self.removed(series_id, &bytes, &mut pending);
                            true
                        }
                        Step::Change(Err(RecvError::Lagged(skipped))) => {
                            tracing::debug!(
                                peer = %self.peer_name(),
                                skipped,
                                "gnmi ON_CHANGE subscriber fell behind the broadcast; resyncing"
                            );
                            self.resync(&mut pending)
                        }
                        Step::Change(Err(RecvError::Closed)) => false,
                    };
                    if !served {
                        break Some(target_stopped());
                    }
                    if was_empty && !pending.is_empty() {
                        waiting_since = Instant::now();
                    }
                    #[cfg(test)]
                    self.probe.max_queued_values.fetch_max(
                        pending.queued_values(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        };
        draining.abort();
        if let Some(status) = ending {
            let _ = self.ending.send(status);
        }
    }

    /// The client's address, or `unknown`.
    fn peer_name(&self) -> String {
        self.peer
            .map_or_else(|| "unknown".to_string(), |peer| peer.to_string())
    }

    /// The latest value of every matching series (none with
    /// `updates_only`), then the sync marker. Each series seen is entered in
    /// the ON_CHANGE set's match cache, with the version of the value
    /// queued for it, and each series queued is recorded as sent by every
    /// suppressing sample group that selects it. `None` once the target is
    /// gone.
    fn initial_sync(&mut self) -> Option<Pending> {
        let mut pending = Pending::default();
        if !self.updates_only {
            let target = self.target.upgrade()?;
            let snapshot = target.snapshot();
            drop(target);
            for series in snapshot {
                let queue = self.matcher.matches(series.path.path());
                if let Some(set) = self.on_change.as_mut() {
                    let seen =
                        set.seen(series.series_id, series.kind, || Some(series.path.clone()));
                    if queue {
                        seen.queued = series.version;
                    }
                }
                if queue {
                    record_sent(&mut self.samples, &series);
                    pending.push_value(series.series_id, series.latest);
                }
            }
        }
        pending.push_response(sync_response());
        Some(pending)
    }

    /// Queue a delete for every series a due suppressing group had sent that
    /// has left the table, then the latest value of every series a due
    /// sample group sends, and move each due group to its next period. A
    /// value for a path published again after its series left follows that
    /// series' delete. `false` once the target is gone.
    fn sample(&mut self, pending: &mut Pending) -> bool {
        #[cfg(test)]
        self.probe
            .samples
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(due) = due(&mut self.samples, &self.target) else {
            return false;
        };
        for (id, path) in due.removed {
            self.queue_delete(id, &delete_notification(path.path()), pending);
        }
        for series in due.series {
            pending.push_response(update_response(&series.latest));
        }
        true
    }

    /// Queue the latest value of every series a due heartbeat group matches,
    /// changed or not, recording its version in the ON_CHANGE set, and move
    /// each due group to its next period. `false` once the target is gone.
    fn heartbeat(&mut self, pending: &mut Pending) -> bool {
        #[cfg(test)]
        self.probe.before_heartbeat.run();
        let Some(due) = due(&mut self.heartbeats, &self.target) else {
            return false;
        };
        for series in due.series {
            if let Some(set) = self.on_change.as_mut() {
                set.seen(series.series_id, series.kind, || Some(series.path.clone()))
                    .queued = series.version;
            }
            pending.push_value(series.series_id, series.latest);
        }
        true
    }

    /// Queue `update` when the ON_CHANGE set matches its series and its
    /// version is newer than that of the series' value last queued; a series
    /// not seen before is matched from the path decoded from the update.
    /// Prunes the match cache of unmatched series when it has outgrown its
    /// limit. `false` once the target is gone.
    fn offer(&mut self, update: SeriesUpdate, pending: &mut Pending) -> bool {
        let Some(set) = self.on_change.as_mut() else {
            return true;
        };
        let SeriesUpdate {
            series_id,
            kind,
            version,
            bytes,
        } = update;
        let seen = set.seen(series_id, kind, || PathKey::decode(&bytes).ok());
        if seen.matches && version > seen.queued {
            seen.queued = version;
            pending.push_value(series_id, bytes);
        }
        if set.matched.len() > set.prune_at {
            #[cfg(test)]
            self.probe
                .prunes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(target) = self.target.upgrade() else {
                return false;
            };
            let snapshot = target.snapshot();
            drop(target);
            set.prune_unmatched(&snapshot);
        }
        true
    }

    /// Handle the broadcast's removal of series `id`: the ON_CHANGE set
    /// forgets the series and, when it matched, queues `delete`.
    fn removed(&mut self, id: SeriesId, delete: &Bytes, pending: &mut Pending) {
        let matched = self
            .on_change
            .as_mut()
            .and_then(|set| set.matched.remove(&id))
            .is_some_and(|seen| seen.matches);
        if matched {
            self.queue_delete(id, delete, pending);
        }
    }

    /// Queue `delete` for series `id` in place of any value waiting for it,
    /// and forget the series in the ON_CHANGE set and every suppressing
    /// sample group, so no other part of the list sends a second delete.
    fn queue_delete(&mut self, id: SeriesId, delete: &Bytes, pending: &mut Pending) {
        pending.push_delete(id, delete);
        if let Some(set) = self.on_change.as_mut() {
            set.matched.remove(&id);
        }
        for sent in self.samples.iter_mut().filter_map(|g| g.sent.as_mut()) {
            sent.remove(&id);
        }
    }

    /// Move the ON_CHANGE set to a receiver at the broadcast's tail, dropping
    /// the changes it fell behind on, then queue the latest value of every
    /// series it matches, recording each one's version, and prune its match
    /// cache to the series in the table. `false` once the target is gone.
    fn resync(&mut self, pending: &mut Pending) -> bool {
        #[cfg(test)]
        self.probe
            .resyncs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(set) = self.on_change.as_mut() else {
            return true;
        };
        set.rx = set.rx.resubscribe();
        #[cfg(test)]
        self.probe.after_resubscribe.run();
        let Some(target) = self.target.upgrade() else {
            return false;
        };
        let snapshot = target.snapshot();
        drop(target);
        set.prune(&snapshot);
        for series in snapshot {
            let seen = set.seen(series.series_id, series.kind, || Some(series.path.clone()));
            if seen.matches {
                seen.queued = series.version;
                pending.push_value(series.series_id, series.latest);
            }
        }
        true
    }
}

/// What the due groups of a list send at one turn.
struct Due {
    /// Every series a due group sends, once each.
    series: Vec<SeriesSnapshot>,
    /// Every series a due suppressing group had sent that is no longer in
    /// the table, once each, with its path.
    removed: Vec<(SeriesId, PathKey)>,
}

/// What the due groups of `groups` send, with each due group moved to its
/// next period. A group without suppression sends every series it selects. A
/// suppressing group sends a series it selects only when the series'
/// version differs from the one it last sent, records each version it
/// sends, and reports, then forgets, each series it had sent that is gone
/// from the table. `None` once the target is gone.
fn due(groups: &mut [Group], target: &Weak<GnmiTarget>) -> Option<Due> {
    let now = Instant::now();
    let target = target.upgrade()?;
    let snapshot = target.snapshot();
    drop(target);
    let mut send = vec![false; snapshot.len()];
    let mut live: Option<HashSet<SeriesId>> = None;
    let mut removed = Vec::new();
    for group in groups.iter_mut().filter(|g| g.next <= now) {
        for (series, send) in snapshot.iter().zip(send.iter_mut()) {
            if !group.selector.selects(&series.path, series.kind) {
                continue;
            }
            match group.sent.as_mut() {
                None => *send = true,
                Some(sent) => {
                    let unchanged = sent
                        .get(&series.series_id)
                        .is_some_and(|s| s.version == series.version);
                    if !unchanged {
                        *send = true;
                        sent.insert(
                            series.series_id,
                            Sent {
                                version: series.version,
                                path: series.path.clone(),
                            },
                        );
                    }
                }
            }
        }
        if let Some(sent) = group.sent.as_mut() {
            let live = live.get_or_insert_with(|| snapshot.iter().map(|s| s.series_id).collect());
            sent.retain(|id, s| {
                let present = live.contains(id);
                if !present {
                    removed.push((*id, s.path.clone()));
                }
                present
            });
        }
        group.next = next_due(group.next, group.period, now);
    }
    removed.sort_unstable_by_key(|(id, _)| *id);
    removed.dedup_by_key(|(id, _)| *id);
    let series = snapshot
        .into_iter()
        .zip(send)
        .filter_map(|(series, send)| send.then_some(series))
        .collect();
    Some(Due { series, removed })
}

/// Record `series` as sent by every suppressing group of `groups` that
/// selects it.
fn record_sent(groups: &mut [Group], series: &SeriesSnapshot) {
    for group in groups.iter_mut() {
        let Some(sent) = group.sent.as_mut() else {
            continue;
        };
        if group.selector.selects(&series.path, series.kind) {
            sent.insert(
                series.series_id,
                Sent {
                    version: series.version,
                    path: series.path.clone(),
                },
            );
        }
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
    use std::sync::{Arc, Mutex, RwLock};

    use prost::Message;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::StreamExt;
    use tokio_util::sync::CancellationToken;
    use tonic::transport::Channel;

    use super::*;
    use crate::config::{BaseScheduleConfig, ScenarioConfig};
    use crate::encoder::gnmi::path::parse_client_path;
    use crate::encoder::gnmi::proto::subscribe_response;
    use crate::encoder::gnmi::proto::typed_value::Value;
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig, GnmiValueType};
    use crate::encoder::{Encoder, EncoderConfig};
    use crate::generator::GeneratorConfig;
    use crate::model::metric::{Labels, MetricEvent};
    use crate::schedule::stats::ScenarioStats;
    use crate::schedule::LaunchId;
    use crate::sink::gnmi::target::{Owner, PathKey, Published, DRAIN_GRACE};
    use crate::sink::gnmi::{registry, GnmiTargetConfig, GnmiTargetSink, MAX_SUBSCRIBER_BUFFER};
    use crate::sink::{create_sink, Sink, SinkConfig};
    use crate::{OnSinkError, SondaError};

    /// The template every test encoder renders.
    const TEMPLATE: &str = "/interfaces/interface[name={ifName}]/state/counters/{name}";

    /// How long a test waits for one response before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// How long a test reads with nothing arriving before it takes the
    /// stream to be quiet.
    const QUIET: Duration = Duration::from_millis(100);

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

    /// `encoder_config` with `in_octets` typed `uint` and `oper_status` an
    /// enum of `UP` (1) and `DOWN` (2).
    fn typed_config() -> GnmiEncoderConfig {
        GnmiEncoderConfig {
            values: HashMap::from([
                ("in_octets".to_string(), GnmiValueType::Uint),
                (
                    "oper_status".to_string(),
                    GnmiValueType::Enum(std::collections::BTreeMap::from([
                        (1, "UP".to_string()),
                        (2, "DOWN".to_string()),
                    ])),
                ),
            ]),
            ..encoder_config()
        }
    }

    /// A sink on its own ephemeral port, with its config adjusted by
    /// `adjust`.
    async fn sink_with(adjust: impl FnOnce(&mut GnmiTargetConfig)) -> GnmiTargetSink {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        adjust(&mut cfg);
        GnmiTargetSink::new(&cfg)
            .await
            .expect("bind an ephemeral port")
    }

    /// The event `name{ifName=if_name}` and its encoded notification.
    fn encoded(name: &str, if_name: &str, value: f64) -> (MetricEvent, Vec<u8>) {
        encoded_with(&encoder_config(), name, if_name, value)
    }

    /// The event `name{ifName=if_name}` and its notification encoded with
    /// `config`.
    fn encoded_with(
        config: &GnmiEncoderConfig,
        name: &str,
        if_name: &str,
        value: f64,
    ) -> (MetricEvent, Vec<u8>) {
        let encoder = GnmiEncoder::new(config).expect("valid encoder");
        let event = MetricEvent::new(
            name.to_string(),
            value,
            Labels::from_pairs(&[("ifName", if_name)]).expect("valid labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        (event, buf)
    }

    /// Publish `name` for interface `if_name` through `sink`.
    async fn publish(sink: &mut GnmiTargetSink, name: &str, if_name: &str, value: f64) {
        let (event, buf) = encoded(name, if_name, value);
        sink.write_metric_event(&event, &buf)
            .await
            .expect("published");
    }

    /// Publish `name` for interface `if_name` through `sink`, encoded with
    /// [`typed_config`].
    async fn publish_typed(sink: &mut GnmiTargetSink, name: &str, if_name: &str, value: f64) {
        let (event, buf) = encoded_with(&typed_config(), name, if_name, value);
        sink.write_metric_event(&event, &buf)
            .await
            .expect("published");
    }

    /// Publish `in_octets` for interface `eth<i>` at `value` straight into
    /// `target`, as `owner`, into series `id` when given.
    fn publish_as(
        target: &GnmiTarget,
        owner: Owner,
        id: Option<SeriesId>,
        i: usize,
        value: f64,
    ) -> (SeriesId, Published) {
        let (_, buf) = encoded("in_octets", &format!("eth{i}"), value);
        let key = PathKey::decode(&buf).expect("a notification");
        target
            .publish(owner, id, &key, &buf)
            .expect("the owner's path")
    }

    /// Publish `name` for interface `if_name` straight into `target`, as a
    /// launch of its own. Returns whether any broadcast receiver existed.
    fn publish_into(target: &GnmiTarget, name: &str, if_name: &str) -> bool {
        let (_, buf) = encoded(name, if_name, 1.0);
        let key = PathKey::decode(&buf).expect("a notification");
        let (_, published) = target
            .publish(Owner::from(LaunchId::next()), None, &key, &buf)
            .expect("a free path");
        published.receivers
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

    /// A STREAM list of one ON_CHANGE subscription to `path`, with
    /// `heartbeat` as its heartbeat interval.
    fn on_change(path: &str, heartbeat: Duration) -> proto::SubscriptionList {
        let mut list = stream(path, Duration::ZERO);
        list.subscription[0].mode = SubscriptionMode::OnChange as i32;
        list.subscription[0].heartbeat_interval = heartbeat.as_nanos() as u64;
        list
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
    async fn eventually(done: impl FnMut() -> bool) -> bool {
        within(PATIENCE, done).await
    }

    /// Wait until `done` holds, polling; false if it never does within
    /// `limit`.
    async fn within(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
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

    /// The next response: an update's series path and value, or `None` for
    /// the sync marker.
    async fn next_value(client: &mut Client) -> Option<(String, Value)> {
        value_within(client, PATIENCE)
            .await
            .expect("a response in time")
    }

    /// The next response as [`next_value`] reads it, or `None` when none
    /// arrives within `wait`.
    async fn value_within(client: &mut Client, wait: Duration) -> Option<Option<(String, Value)>> {
        let message = tokio::time::timeout(wait, client.responses.message())
            .await
            .ok()?
            .expect("no error on the stream")
            .expect("the stream is open");
        Some(match message.response {
            Some(subscribe_response::Response::SyncResponse(true)) => None,
            Some(subscribe_response::Response::Update(n)) => Some((series_path(&n), value_of(&n))),
            other => panic!("unexpected response {other:?}"),
        })
    }

    /// The value of a notification's first update.
    fn value_of(notification: &proto::Notification) -> Value {
        notification
            .update
            .first()
            .and_then(|u| u.val.clone())
            .and_then(|v| v.value)
            .expect("an update with a value")
    }

    /// Every response received until `deadline`, as [`next_value`] reads
    /// them.
    async fn values_until(client: &mut Client, deadline: Instant) -> Vec<Option<(String, Value)>> {
        let mut values = Vec::new();
        while let Ok(message) = tokio::time::timeout_at(deadline, client.responses.message()).await
        {
            let response = message.expect("no error on the stream").expect("open");
            values.push(match response.response {
                Some(subscribe_response::Response::SyncResponse(true)) => None,
                Some(subscribe_response::Response::Update(n)) => {
                    Some((series_path(&n), value_of(&n)))
                }
                other => panic!("unexpected response {other:?}"),
            });
        }
        values
    }

    /// How many of `values` are updates for `series`.
    fn updates_for(values: &[Option<(String, Value)>], series: &str) -> usize {
        values
            .iter()
            .filter(|v| v.as_ref().is_some_and(|(path, _)| path == series))
            .count()
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
        let first = open(addr, list()).await.expect("first subscriber");
        let _second = open(addr, list()).await.expect("second subscriber");
        assert_eq!(sink.target().subscriber_count(), 2);

        let status = match open(addr, list()).await {
            Ok(_) => panic!("a third subscriber is admitted"),
            Err(status) => status,
        };
        assert_eq!(status.code(), tonic::Code::ResourceExhausted, "{status}");
        assert!(status.message().contains("max_subscribers"), "{status}");
        assert_eq!(sink.target().subscriber_count(), 2);

        drop(first);
        let target = Arc::clone(sink.target());
        assert!(eventually(|| target.subscriber_count() == 1).await);
        let _third = open(addr, list())
            .await
            .expect("the place the first gave back is taken");
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
        let channel = connect_with_small_window(sink.target().local_addr()).await;
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
    /// receives every matching series and the sync marker, then samples:
    /// the request stream ending does not end the subscription. Dropping
    /// the client does.
    #[tokio::test]
    async fn a_half_closed_request_stream_keeps_its_subscription() {
        const SERIES: usize = 20;
        let mut sink = sink(None, Some("100ms")).await;
        for i in 0..SERIES {
            publish(&mut sink, "in_octets", &format!("eth{i}"), 1.0).await;
        }
        assert_eq!(sink.target().series_count(), SERIES);
        let mut client = open_half_closed(
            sink.target().local_addr(),
            stream("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        let mut synced = std::collections::HashSet::new();
        for i in 0..SERIES {
            match next(&mut client).await {
                Received::Update(path) => synced.insert(path),
                other => panic!("sync update {i} of {SERIES}: {other:?}"),
            };
        }
        assert_eq!(synced.len(), SERIES, "the sync carries every series once");
        assert_eq!(next(&mut client).await, Received::Sync);
        for batch in 1..=2 {
            for i in 0..SERIES {
                assert!(
                    matches!(next(&mut client).await, Received::Update(_)),
                    "sample {batch}, update {i} of {SERIES}"
                );
            }
        }
        assert_eq!(sink.target().subscriber_count(), 1);

        drop(client);
        let target = Arc::clone(sink.target());
        assert!(
            eventually(|| target.subscriber_count() == 0).await,
            "still {} subscribers",
            target.subscriber_count()
        );
    }

    /// A list that sets no encoding carries encoding 0 (JSON) and is served
    /// in PROTO.
    #[tokio::test]
    async fn an_unset_encoding_is_served_in_proto() {
        let mut sink = sink(None, None).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "in_octets", "eth1", 2.0).await;
        assert_eq!(sink.target().series_count(), 2);
        let list = proto::SubscriptionList {
            mode: subscription_list::Mode::Once as i32,
            ..stream("/interfaces", Duration::ZERO)
        };
        let list = proto::SubscriptionList {
            encoding: proto::SubscriptionList::default().encoding,
            ..list
        };
        assert_eq!(list.encoding, proto::Encoding::Json as i32);

        let mut client = open(sink.target().local_addr(), list)
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

    /// A SAMPLE subscription takes a subscriber place, not a broadcast
    /// receiver: it counts in `subscriber_count()`, and a publish finds
    /// nobody on the broadcast.
    #[tokio::test]
    async fn sample_subscribers_take_a_place_not_a_broadcast_receiver() {
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
        assert!(
            !publish_into(sink.target(), "out_octets", "eth0"),
            "a publish found a broadcast receiver"
        );
    }

    /// A target on an ephemeral port that pings a connection silent for
    /// `keepalive` and closes it when the ping goes unanswered as long.
    async fn target_with_keepalive(keepalive: Duration, max_subscribers: usize) -> Arc<GnmiTarget> {
        target_with(max_subscribers, |settings| {
            settings.keepalive_interval = keepalive;
            settings.keepalive_timeout = keepalive;
        })
        .await
    }

    /// A target on an ephemeral port serving `max_subscribers`, with its
    /// settings adjusted by `adjust`.
    async fn target_with(
        max_subscribers: usize,
        adjust: impl FnOnce(&mut crate::sink::gnmi::TargetSettings),
    ) -> Arc<GnmiTarget> {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        cfg.max_subscribers = Some(max_subscribers);
        let mut settings = cfg.settings().expect("valid settings");
        adjust(&mut settings);
        GnmiTarget::bind(settings)
            .await
            .expect("bind an ephemeral port")
    }

    /// A connection to `addr` whose HTTP/2 windows are 64 KiB, so a few
    /// hundred unread updates fill them.
    async fn connect_with_small_window(addr: SocketAddr) -> Channel {
        tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("a valid uri")
            .initial_stream_window_size(65_535)
            .initial_connection_window_size(65_535)
            .connect()
            .await
            .expect("connect to the target")
    }

    /// The series, sample period and stall floor of the stall tests: 2,000
    /// updates per sample are far more than a 64 KiB window holds, and the
    /// stall limit is max(3 × 50 ms, 200 ms) = 200 ms.
    const STALL_SERIES: usize = 2_000;
    const STALL_PERIOD: Duration = Duration::from_millis(50);
    const STALL_TEST_FLOOR: Duration = Duration::from_millis(200);

    /// A target holding [`STALL_SERIES`] series, sampling as often as
    /// [`STALL_PERIOD`], with a stall floor of [`STALL_TEST_FLOOR`].
    async fn stall_target() -> Arc<GnmiTarget> {
        let target = target_with(1, |settings| {
            settings.sample_floor = STALL_PERIOD;
            settings.stall_floor = STALL_TEST_FLOOR;
        })
        .await;
        for i in 0..STALL_SERIES {
            publish_into(&target, "in_octets", &format!("eth{i}"));
        }
        assert_eq!(target.series_count(), STALL_SERIES);
        target
    }

    #[test]
    fn stall_limit_is_three_longest_periods_or_the_floor() {
        let floor = Duration::from_secs(10);
        let s = Duration::from_secs;
        assert_eq!(stall_limit([s(1), s(5)].into_iter(), floor), s(15));
        assert_eq!(stall_limit([s(1), s(2)].into_iter(), floor), floor);
        assert_eq!(stall_limit(std::iter::empty(), floor), floor, "a ONCE list");
    }

    /// A client that keeps its connection open but never reads its
    /// responses is ended once responses have waited the stall limit: its
    /// place is given back, a new subscriber is admitted, and when it does
    /// read, its stream ends with the stall status.
    #[tokio::test]
    async fn a_subscriber_that_drains_nothing_is_ended_as_stalled() {
        let target = stall_target().await;
        let mut stalled = open_on(
            connect_with_small_window(target.local_addr()).await,
            stream("/interfaces", STALL_PERIOD),
            true,
        )
        .await
        .expect("subscribed");
        assert_eq!(target.subscriber_count(), 1);
        assert!(
            within(2 * STALL_TEST_FLOOR, || target.subscriber_count() == 0).await,
            "a client that drains nothing still holds its place"
        );
        let _next = open(target.local_addr(), stream("/interfaces", STALL_PERIOD))
            .await
            .expect("the stalled client's place was given back");
        assert_eq!(target.subscriber_count(), 1);

        let mut updates = 0usize;
        let ending = loop {
            match tokio::time::timeout(PATIENCE, stalled.responses.message())
                .await
                .expect("a response in time")
            {
                Ok(Some(_)) => updates += 1,
                ending => break ending,
            }
        };
        assert!(updates > 0, "the stalled client was sent updates");
        let status = ending.expect_err("the stream ends with a status, not OK");
        assert_eq!(status.code(), tonic::Code::Unavailable, "{status}");
        assert!(status.message().contains("stalled"), "{status}");
    }

    /// A client whose list's longest sample period, times three, exceeds the
    /// floor is held to that longer limit: with a 100 ms period and a
    /// 200 ms floor, a client that never reads still holds its place at
    /// 250 ms and has lost it by 600 ms.
    #[tokio::test]
    async fn the_stall_limit_is_three_of_the_longest_sample_periods() {
        let period = 2 * STALL_PERIOD;
        let target = stall_target().await;
        let opened = Instant::now();
        let _stalled = open_on(
            connect_with_small_window(target.local_addr()).await,
            stream("/interfaces", period),
            true,
        )
        .await
        .expect("subscribed");
        assert_eq!(target.subscriber_count(), 1);
        tokio::time::sleep_until(opened + Duration::from_millis(250)).await;
        assert_eq!(
            target.subscriber_count(),
            1,
            "ended before 3 × the 100 ms period, at the 200 ms floor"
        );
        assert!(
            within(Duration::from_millis(350), || target.subscriber_count()
                == 0)
            .await,
            "still held at 600 ms, past the 300 ms limit"
        );
    }

    /// A client that reads slowly but without pause, with the target always
    /// waiting on it, stays subscribed across several stall limits.
    #[tokio::test]
    async fn a_slow_but_moving_reader_is_not_ended() {
        let target = stall_target().await;
        let mut client = open_on(
            connect_with_small_window(target.local_addr()).await,
            stream("/interfaces", STALL_PERIOD),
            true,
        )
        .await
        .expect("subscribed");
        let started = Instant::now();
        let mut read = 0usize;
        while started.elapsed() < 5 * STALL_TEST_FLOOR {
            for _ in 0..50 {
                let message = tokio::time::timeout(PATIENCE, client.responses.message())
                    .await
                    .expect("a response in time")
                    .expect("no error on the stream");
                assert!(message.is_some(), "the stream ended after {read} responses");
                read += 1;
            }
            assert_eq!(target.subscriber_count(), 1, "ended after {read} responses");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let kept_up = STALL_SERIES * (started.elapsed().as_millis() as usize)
            / STALL_PERIOD.as_millis() as usize;
        assert!(
            read < kept_up / 2,
            "the reader kept up ({read} of {kept_up}), so the target never waited on it"
        );
    }

    /// A proxy on an ephemeral port forwarding one connection to `target`
    /// in both directions, each read `delay` after it arrives, until `cut`
    /// turns true; from then on it forwards nothing and keeps both sockets
    /// open.
    async fn proxy(target: SocketAddr, cut: watch::Receiver<bool>, delay: Duration) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("bound");
        tokio::spawn(async move {
            let (client, _) = listener.accept().await.expect("a client");
            let server = TcpStream::connect(target).await.expect("the target");
            let (client_read, client_write) = client.into_split();
            let (server_read, server_write) = server.into_split();
            tokio::join!(
                pipe(client_read, server_write, cut.clone(), delay),
                pipe(server_read, client_write, cut, delay)
            );
        });
        addr
    }

    /// Copy `from` to `to`, each read `delay` after it arrives, until `cut`
    /// turns true, then hold both open.
    async fn pipe(
        mut from: OwnedReadHalf,
        mut to: OwnedWriteHalf,
        mut cut: watch::Receiver<bool>,
        delay: Duration,
    ) {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = tokio::select! {
                read = from.read(&mut buf) => match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                },
                _ = cut.wait_for(|cut| *cut) => break,
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if *cut.borrow() || to.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        std::future::pending::<()>().await;
    }

    /// A client whose connection goes silent, answering no ping, is closed
    /// after the keepalive timeout: its subscription ends and its place is
    /// given back for a new subscriber.
    #[tokio::test]
    async fn a_silent_connection_loses_its_subscription_to_keepalive() {
        let target = target_with_keepalive(Duration::from_millis(200), 1).await;
        publish_into(&target, "in_octets", "eth0");
        let (cut, uncut) = watch::channel(false);
        let via = proxy(target.local_addr(), uncut, Duration::ZERO).await;
        let mut client = open(via, stream("/interfaces", Duration::ZERO))
            .await
            .expect("subscribed through the proxy");
        assert_eq!(
            next(&mut client).await,
            Received::Update(counter("eth0", "in-octets"))
        );
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(target.subscriber_count(), 1);

        cut.send_replace(true);
        assert!(
            within(Duration::from_secs(2), || target.subscriber_count() == 0).await,
            "the silent client still holds its place"
        );
        let _again = open(target.local_addr(), stream("/interfaces", Duration::ZERO))
            .await
            .expect("the place was given back");
        assert_eq!(target.subscriber_count(), 1);
        drop(client);
    }

    /// A client that answers pings keeps its subscription across the same
    /// keepalive interval and timeout. It is reached through a proxy that
    /// delays each direction by 25 ms, so every ping's answer takes at least
    /// 50 ms to arrive.
    #[tokio::test]
    async fn a_client_answering_pings_keeps_its_subscription() {
        let target = target_with_keepalive(Duration::from_millis(200), 1).await;
        publish_into(&target, "in_octets", "eth0");
        let (_cut, uncut) = watch::channel(false);
        let via = proxy(target.local_addr(), uncut, Duration::from_millis(25)).await;
        let mut client = open(via, stream("/interfaces", Duration::ZERO))
            .await
            .expect("subscribed through the proxy");
        let expected = Received::Update(counter("eth0", "in-octets"));
        assert_eq!(next(&mut client).await, expected);
        assert_eq!(next(&mut client).await, Received::Sync);

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(target.subscriber_count(), 1, "still subscribed");
        assert_eq!(next(&mut client).await, expected, "and still sampled");
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

    /// An ON_CHANGE subscription receives the initial sync and the sync
    /// marker, then one update per change of a series it matches: the first
    /// response after many publishes of an unchanged value is the next
    /// change, and an unchanged value sends nothing.
    #[tokio::test]
    async fn on_change_sends_the_sync_then_one_update_per_change() {
        const UNCHANGED: usize = 20;
        let mut sink = sink(None, None).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "out_octets", "eth0", 1.0).await;
        let mut client = open(
            sink.target().local_addr(),
            on_change(
                "/interfaces/interface/state/counters/in-octets",
                Duration::ZERO,
            ),
        )
        .await
        .expect("subscribed");
        let series = counter("eth0", "in-octets");
        assert_eq!(
            next_value(&mut client).await,
            Some((series.clone(), Value::DoubleVal(1.0)))
        );
        assert_eq!(next_value(&mut client).await, None, "the sync marker");

        for value in [2.0, 3.0, 4.0] {
            for _ in 0..UNCHANGED {
                publish(&mut sink, "in_octets", "eth0", value - 1.0).await;
                publish(&mut sink, "out_octets", "eth0", value).await;
            }
            publish(&mut sink, "in_octets", "eth0", value).await;
            assert_eq!(
                next_value(&mut client).await,
                Some((series.clone(), Value::DoubleVal(value))),
                "the first response after {UNCHANGED} unchanged publishes is the change"
            );
        }
        for _ in 0..UNCHANGED {
            publish(&mut sink, "in_octets", "eth0", 4.0).await;
        }
        let quiet = values_until(&mut client, Instant::now() + Duration::from_millis(300)).await;
        assert!(quiet.is_empty(), "an unchanged value sent {quiet:?}");
    }

    /// A series first published after an ON_CHANGE subscription started is
    /// delivered when it matches, and never when it does not.
    #[tokio::test]
    async fn on_change_delivers_a_new_series_only_when_it_matches() {
        let mut sink = sink(None, None).await;
        let mut client = open(
            sink.target().local_addr(),
            on_change(
                "/interfaces/interface/state/counters/in-octets",
                Duration::ZERO,
            ),
        )
        .await
        .expect("subscribed");
        assert_eq!(
            next_value(&mut client).await,
            None,
            "an empty table syncs at once"
        );

        publish(&mut sink, "out_octets", "eth1", 1.0).await;
        publish(&mut sink, "in_octets", "eth1", 2.0).await;
        assert_eq!(
            next_value(&mut client).await,
            Some((counter("eth1", "in-octets"), Value::DoubleVal(2.0))),
            "the new matching series, and not the one published before it"
        );
        publish(&mut sink, "out_octets", "eth1", 3.0).await;
        publish(&mut sink, "in_octets", "eth1", 4.0).await;
        assert_eq!(
            next_value(&mut client).await,
            Some((counter("eth1", "in-octets"), Value::DoubleVal(4.0))),
            "the unmatched series' change was not sent"
        );
    }

    /// `heartbeat_interval` re-sends the latest value on its interval while
    /// the value holds; an interval below the sample floor is the floor.
    #[tokio::test]
    async fn heartbeat_resends_the_latest_value_without_a_change() {
        let mut sink = sink(None, Some("100ms")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        let addr = sink.target().local_addr();
        let series = Some((counter("eth0", "in-octets"), Value::DoubleVal(1.0)));
        let mut beating = open(addr, on_change("/interfaces", Duration::from_millis(250)))
            .await
            .expect("subscribed");
        let mut floored = open(addr, on_change("/interfaces", Duration::from_millis(1)))
            .await
            .expect("subscribed");
        for client in [&mut beating, &mut floored] {
            assert_eq!(next_value(client).await, series);
            assert_eq!(next_value(client).await, None, "the sync marker");
        }

        let deadline = Instant::now() + Duration::from_millis(1_100);
        let (beats, floor_beats) = tokio::join!(
            values_until(&mut beating, deadline),
            values_until(&mut floored, deadline)
        );
        assert!(
            (3..=5).contains(&beats.len()),
            "{} heartbeats in 1.1 s at 250 ms: {beats:?}",
            beats.len()
        );
        assert!(
            (8..=12).contains(&floor_beats.len()),
            "{} heartbeats in 1.1 s at a 1 ms interval and a 100 ms floor",
            floor_beats.len()
        );
        assert!(
            beats.iter().chain(&floor_beats).all(|b| *b == series),
            "every heartbeat is the unchanged latest value"
        );
    }

    /// TARGET_DEFINED serves an enum (`string_val`) series on change and a
    /// counter on the sample period: the counter, changing every 10 ms, is
    /// sent about once per period, and the enum only when it changes.
    #[tokio::test]
    async fn target_defined_serves_enums_on_change_and_the_rest_by_sample() {
        let mut sink = sink(None, Some("200ms")).await;
        publish_typed(&mut sink, "in_octets", "eth0", 1.0).await;
        publish_typed(&mut sink, "oper_status", "eth0", 1.0).await;
        let mut list = stream("/interfaces", Duration::ZERO);
        list.subscription[0].mode = SubscriptionMode::TargetDefined as i32;
        let mut client = open(sink.target().local_addr(), list)
            .await
            .expect("subscribed");
        let (octets, status) = (counter("eth0", "in-octets"), counter("eth0", "oper-status"));
        let mut synced = vec![next_value(&mut client).await, next_value(&mut client).await];
        synced.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(
            synced,
            [
                Some((octets.clone(), Value::UintVal(1))),
                Some((status.clone(), Value::StringVal("UP".to_string()))),
            ]
        );
        assert_eq!(next_value(&mut client).await, None, "the sync marker");

        let started = Instant::now();
        let mut received = Vec::new();
        let mut value = 1.0;
        while started.elapsed() < Duration::from_secs(1) {
            value += 1.0;
            publish_typed(&mut sink, "in_octets", "eth0", value).await;
            received.extend(
                values_until(&mut client, Instant::now() + Duration::from_millis(10)).await,
            );
        }
        let sampled = updates_for(&received, &octets);
        assert!(
            value > 50.0 && (3..=6).contains(&sampled),
            "{sampled} counter updates in 1 s for {value} changes at a 200 ms period"
        );
        assert_eq!(updates_for(&received, &status), 0, "the enum held");
        assert!(!received.contains(&None), "no second sync marker");

        publish_typed(&mut sink, "oper_status", "eth0", 2.0).await;
        loop {
            match next_value(&mut client).await {
                Some((path, value)) if path == status => {
                    assert_eq!(value, Value::StringVal("DOWN".to_string()));
                    break;
                }
                Some(_) => continue,
                None => panic!("a second sync marker"),
            }
        }
    }

    /// A list with a SAMPLE and an ON_CHANGE subscription sends one initial
    /// sync and one sync marker; then the sample group ticks and the
    /// ON_CHANGE subscription sends changes.
    #[tokio::test]
    async fn a_mixed_list_sends_one_sync_marker() {
        let mut sink = sink(None, Some("100ms")).await;
        publish(&mut sink, "in_octets", "eth0", 1.0).await;
        publish(&mut sink, "out_octets", "eth0", 1.0).await;
        let mut list = stream(
            "/interfaces/interface/state/counters/in-octets",
            Duration::ZERO,
        );
        list.subscription.push(
            on_change(
                "/interfaces/interface/state/counters/out-octets",
                Duration::ZERO,
            )
            .subscription
            .remove(0),
        );
        let mut client = open(sink.target().local_addr(), list)
            .await
            .expect("subscribed");
        let (sampled, changing) = (counter("eth0", "in-octets"), counter("eth0", "out-octets"));
        let mut synced = vec![next_value(&mut client).await, next_value(&mut client).await];
        synced.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(
            synced,
            [
                Some((sampled.clone(), Value::DoubleVal(1.0))),
                Some((changing.clone(), Value::DoubleVal(1.0))),
            ]
        );
        assert_eq!(next_value(&mut client).await, None, "the sync marker");

        let rest = values_until(&mut client, Instant::now() + Duration::from_millis(550)).await;
        assert!(!rest.contains(&None), "one sync marker: {rest:?}");
        assert!(
            updates_for(&rest, &sampled) >= 3,
            "the sample group ticks: {rest:?}"
        );
        assert_eq!(updates_for(&rest, &changing), 0, "out-octets held");

        publish(&mut sink, "out_octets", "eth0", 2.0).await;
        loop {
            match next_value(&mut client).await {
                Some((path, value)) if path == changing => {
                    assert_eq!(value, Value::DoubleVal(2.0));
                    break;
                }
                Some(_) => continue,
                None => panic!("a second sync marker"),
            }
        }
    }

    /// An ON_CHANGE subscription that falls more than `subscriber_buffer`
    /// changes behind is resynced: the client ends with the latest value of
    /// every matched series, no series' value ever goes back, and no second
    /// sync marker is sent. Each burst changes `eth0` once, first, and then
    /// every other series many times, so the broadcast's backlog holds older
    /// values of the other series and none of `eth0`'s: a resync that
    /// replayed the backlog would send older values after the latest, and
    /// one that queued nothing would leave `eth0` behind. Each burst is
    /// published without yielding to the single-threaded runtime, so the
    /// subscription cannot read the broadcast until it is over; the client
    /// then reads until nothing has arrived for [`QUIET`] after the latest.
    /// Three bursts each cause a resync.
    #[tokio::test]
    async fn a_lagged_on_change_subscriber_is_resynced_to_the_latest() {
        const SERIES: usize = 5;
        const ROUNDS: usize = 20;
        const BURSTS: usize = 3;
        let mut sink = sink_with(|cfg| cfg.subscriber_buffer = Some(8)).await;
        for i in 0..SERIES {
            publish(&mut sink, "in_octets", &format!("eth{i}"), 0.0).await;
        }
        let mut client = open(
            sink.target().local_addr(),
            on_change("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        let mut seen: HashMap<String, f64> = HashMap::new();
        for i in 0..SERIES {
            let (path, value) = next_value(&mut client).await.expect("a sync update");
            assert_eq!(value, Value::DoubleVal(0.0), "sync update {i}");
            seen.insert(path, 0.0);
        }
        assert_eq!(next_value(&mut client).await, None, "the sync marker");

        let probe = Arc::clone(&sink.target().settings().probe);
        for burst in 0..BURSTS {
            let resyncs = probe.resyncs.load(std::sync::atomic::Ordering::Relaxed);
            let last = ((burst + 1) * ROUNDS) as f64;
            publish(&mut sink, "in_octets", "eth0", last).await;
            for round in 1..=ROUNDS {
                let value = (burst * ROUNDS + round) as f64;
                for i in 1..SERIES {
                    publish(&mut sink, "in_octets", &format!("eth{i}"), value).await;
                }
            }
            loop {
                let at_last = seen.values().all(|v| *v == last);
                let wait = if at_last { QUIET } else { PATIENCE };
                let Some(received) = value_within(&mut client, wait).await else {
                    assert!(at_last, "burst {burst}: no response in time, at {seen:?}");
                    break;
                };
                let Some((path, Value::DoubleVal(value))) = received else {
                    panic!(
                        "burst {burst}: a second sync marker or a non-double value, at {seen:?}"
                    );
                };
                let previous = seen.insert(path.clone(), value).expect("a matched series");
                assert!(
                    value >= previous,
                    "burst {burst}: {path} went back from {previous} to {value}"
                );
            }
            assert!(
                probe.resyncs.load(std::sync::atomic::Ordering::Relaxed) > resyncs,
                "burst {burst}: the subscription lagged and was resynced"
            );
        }
    }

    /// Every response `responses` yields until none arrives for [`QUIET`]:
    /// an update's value, or `None` for the sync marker.
    async fn responses_until_quiet(responses: &mut Responses) -> Vec<Option<Value>> {
        let mut received = Vec::new();
        while let Ok(Some(response)) = tokio::time::timeout(QUIET, responses.next()).await {
            let response = response.expect("no error on the stream");
            let decoded =
                proto::SubscribeResponse::decode(response.0).expect("a subscribe response");
            received.push(match decoded.response {
                Some(subscribe_response::Response::SyncResponse(true)) => None,
                Some(subscribe_response::Response::Update(n)) => Some(value_of(&n)),
                other => panic!("unexpected response {other:?}"),
            });
        }
        received
    }

    /// Sets a flag when dropped, so a thread polling it stops however the
    /// test ends.
    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Subscriptions opened one after another while a series changes about
    /// every 20 µs, from another thread, receive no value twice and never an
    /// older value after a newer one. A change published between a
    /// subscription taking its broadcast receiver and reading its snapshot
    /// is in both, and is sent once. The buffer holds every change, so no
    /// subscription lags and no resync is involved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_change_racing_the_initial_sync_is_sent_once() {
        const SUBSCRIPTIONS: usize = 60;
        const UPDATES: usize = 20;
        let target = target_with(SUBSCRIPTIONS, |settings| {
            settings.subscriber_buffer = MAX_SUBSCRIBER_BUFFER;
        })
        .await;
        let owner = Owner::from(LaunchId::next());
        let (id, _) = publish_as(&target, owner, None, 0, 0.0);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let publisher = {
            let (target, stop) = (Arc::clone(&target), Arc::clone(&stop));
            std::thread::spawn(move || {
                let encoder = GnmiEncoder::new(&encoder_config()).expect("valid encoder");
                let labels = Labels::from_pairs(&[("ifName", "eth0")]).expect("valid labels");
                let mut buf = Vec::new();
                let mut published = 0_u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    published += 1;
                    let event =
                        MetricEvent::new("in_octets".to_string(), published as f64, labels.clone())
                            .expect("valid event");
                    buf.clear();
                    encoder.encode_metric(&event, &mut buf).expect("encodes");
                    let key = PathKey::decode(&buf).expect("a notification");
                    target
                        .publish(owner, Some(id), &key, &buf)
                        .expect("the owner's path");
                    let next = std::time::Instant::now() + Duration::from_micros(20);
                    while std::time::Instant::now() < next {
                        std::hint::spin_loop();
                    }
                }
                published
            })
        };
        let stopping = SetOnDrop(stop);

        let mut duplicates = Vec::new();
        let mut regressions = Vec::new();
        for n in 0..SUBSCRIPTIONS {
            let mut client = open(
                target.local_addr(),
                on_change("/interfaces", Duration::ZERO),
            )
            .await
            .expect("subscribed");
            let Some((_, Value::DoubleVal(mut highest))) = next_value(&mut client).await else {
                panic!("subscription {n}: a sync update with a double");
            };
            assert_eq!(
                next_value(&mut client).await,
                None,
                "subscription {n}: the sync marker"
            );
            for _ in 0..UPDATES {
                let Some((_, Value::DoubleVal(value))) = next_value(&mut client).await else {
                    panic!("subscription {n}: an update with a double");
                };
                if value == highest {
                    duplicates.push((n, value));
                } else if value < highest {
                    regressions.push((n, highest, value));
                }
                highest = highest.max(value);
            }
        }
        drop(stopping);
        let published = publisher.join().expect("the publisher ran");
        assert!(
            published > (SUBSCRIPTIONS * UPDATES) as u64,
            "the premise: the series changed throughout ({published} changes)"
        );
        assert_eq!(
            target
                .settings()
                .probe
                .resyncs
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the premise: no subscription lagged"
        );
        assert!(
            duplicates.is_empty() && regressions.is_empty(),
            "sent again: {duplicates:?}; older after newer: {regressions:?}"
        );
    }

    /// A change published after a subscription takes its broadcast receiver
    /// and before its producer reads the snapshot is in both; it is sent
    /// once, in the initial sync. Each of ten subscriptions receives exactly
    /// the changed value, then the sync marker, then nothing. The clock is
    /// paused, so the quiet wait ends as soon as the subscription has
    /// nothing left to do.
    #[tokio::test(start_paused = true)]
    async fn a_change_between_subscribe_and_the_snapshot_is_sent_once() {
        const SUBSCRIPTIONS: usize = 10;
        let target = target_with(SUBSCRIPTIONS, |_| {}).await;
        let owner = Owner::from(LaunchId::next());
        let (id, _) = publish_as(&target, owner, None, 0, 0.0);
        let list = on_change("/interfaces", Duration::ZERO);
        for n in 1..=SUBSCRIPTIONS {
            let (producer, mut responses) =
                start(Arc::downgrade(&target), None, &list).expect("started");
            publish_as(&target, owner, Some(id), 0, n as f64);
            tokio::spawn(producer.run(tokio::spawn(async {})));
            assert_eq!(
                responses_until_quiet(&mut responses).await,
                [Some(Value::DoubleVal(n as f64)), None],
                "subscription {n}: the changed value once, then the sync marker"
            );
        }
    }

    /// A change published after a resync takes its new broadcast receiver
    /// and before it reads the snapshot is in both; it is sent once. Each of
    /// ten bursts of 20 changes, published without yielding, lags the
    /// subscription (a buffer of 8). The resync's hook publishes one more
    /// change between the two steps, and the client receives exactly that
    /// value. The clock is paused, as above.
    #[tokio::test(start_paused = true)]
    async fn a_change_between_a_resyncs_receiver_and_its_snapshot_is_sent_once() {
        const BURSTS: usize = 10;
        const BURST: usize = 20;
        let target = target_with(1, |settings| settings.subscriber_buffer = 8).await;
        let owner = Owner::from(LaunchId::next());
        let (id, _) = publish_as(&target, owner, None, 0, 0.0);
        let inside = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = Arc::clone(&target.settings().probe);
        probe.after_resubscribe.set({
            let (target, inside) = (Arc::downgrade(&target), Arc::clone(&inside));
            move || {
                if let Some(target) = target.upgrade() {
                    let value = inside.load(std::sync::atomic::Ordering::Relaxed);
                    publish_as(&target, owner, Some(id), 0, value as f64);
                }
            }
        });
        let (producer, mut responses) = start(
            Arc::downgrade(&target),
            None,
            &on_change("/interfaces", Duration::ZERO),
        )
        .expect("started");
        tokio::spawn(producer.run(tokio::spawn(async {})));
        assert_eq!(
            responses_until_quiet(&mut responses).await,
            [Some(Value::DoubleVal(0.0)), None],
            "the initial sync"
        );
        for burst in 1..=BURSTS {
            let base = burst * 100;
            let resyncs = probe.resyncs.load(std::sync::atomic::Ordering::Relaxed);
            inside.store(base + BURST + 1, std::sync::atomic::Ordering::Relaxed);
            for k in 1..=BURST {
                publish_as(&target, owner, Some(id), 0, (base + k) as f64);
            }
            assert_eq!(
                responses_until_quiet(&mut responses).await,
                [Some(Value::DoubleVal((base + BURST + 1) as f64))],
                "burst {burst}: the value published inside the resync, once"
            );
            assert_eq!(
                probe.resyncs.load(std::sync::atomic::Ordering::Relaxed),
                resyncs + 1,
                "burst {burst}: the premise: one resync"
            );
        }
    }

    /// A heartbeat records the version it sends, so a change still on the
    /// broadcast whose version is not newer than the heartbeat's value is
    /// not sent after it. In each of 40 rounds the paused clock is moved to
    /// the heartbeat, and the heartbeat's hook publishes two changes before
    /// the heartbeat reads the snapshot: the snapshot holds the newer one,
    /// and both are still on the broadcast. The client receives exactly the
    /// newer value, once. A last round with no change receives the
    /// heartbeat alone.
    #[tokio::test(start_paused = true)]
    async fn a_change_a_heartbeat_already_holds_is_not_sent_again() {
        const ROUNDS: usize = 40;
        const HEARTBEAT: Duration = Duration::from_secs(10);
        let target = target_with(1, |_| {}).await;
        let owner = Owner::from(LaunchId::next());
        let (id, _) = publish_as(&target, owner, None, 0, 0.0);
        let round = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        target.settings().probe.before_heartbeat.set({
            let (target, round) = (Arc::downgrade(&target), Arc::clone(&round));
            move || {
                let round = round.load(std::sync::atomic::Ordering::Relaxed);
                if let Some(target) = target.upgrade().filter(|_| round > 0) {
                    publish_as(&target, owner, Some(id), 0, (round * 10 + 1) as f64);
                    publish_as(&target, owner, Some(id), 0, (round * 10 + 2) as f64);
                }
            }
        });
        let (producer, mut responses) = start(
            Arc::downgrade(&target),
            None,
            &on_change("/interfaces", HEARTBEAT),
        )
        .expect("started");
        tokio::spawn(producer.run(tokio::spawn(async {})));
        assert_eq!(
            responses_until_quiet(&mut responses).await,
            [Some(Value::DoubleVal(0.0)), None],
            "the initial sync"
        );
        for n in 1..=ROUNDS {
            round.store(n, std::sync::atomic::Ordering::Relaxed);
            tokio::time::advance(HEARTBEAT).await;
            assert_eq!(
                responses_until_quiet(&mut responses).await,
                [Some(Value::DoubleVal((n * 10 + 2) as f64))],
                "round {n}: the newer change, once"
            );
        }
        round.store(0, std::sync::atomic::Ordering::Relaxed);
        tokio::time::advance(HEARTBEAT).await;
        assert_eq!(
            responses_until_quiet(&mut responses).await,
            [Some(Value::DoubleVal((ROUNDS * 10 + 2) as f64))],
            "the premise: a heartbeat falls due each period"
        );
    }

    /// A SAMPLE list for `path` at `period` with `suppress_redundant`, and
    /// `heartbeat` as its heartbeat interval (0 for none).
    fn suppressed(path: &str, period: Duration, heartbeat: Duration) -> proto::SubscriptionList {
        let mut list = stream(path, period);
        list.subscription[0].suppress_redundant = true;
        list.subscription[0].heartbeat_interval = heartbeat.as_nanos() as u64;
        list
    }

    /// One response as the delete tests read it.
    #[derive(Debug, PartialEq)]
    enum Got {
        /// An update: its series path and value.
        Value(String, Value),
        /// A delete: the full path it names.
        Delete(String),
        Sync,
    }

    /// Every response received until `deadline`, deletes included.
    async fn got_until(client: &mut Client, deadline: Instant) -> Vec<Got> {
        let mut got = Vec::new();
        while let Ok(message) = tokio::time::timeout_at(deadline, client.responses.message()).await
        {
            let response = message.expect("no error on the stream").expect("open");
            got.push(match response.response {
                Some(subscribe_response::Response::SyncResponse(true)) => Got::Sync,
                Some(subscribe_response::Response::Update(n)) if !n.delete.is_empty() => {
                    assert_eq!(n.delete.len(), 1, "one deleted path");
                    assert!(n.update.is_empty(), "a delete carries no update");
                    let prefix = n.prefix.clone().unwrap_or_default();
                    Got::Delete(
                        proto::Path {
                            origin: prefix.origin,
                            elem: n.delete[0].elem.clone(),
                            target: prefix.target,
                        }
                        .to_string(),
                    )
                }
                Some(subscribe_response::Response::Update(n)) => {
                    Got::Value(series_path(&n), value_of(&n))
                }
                other => panic!("unexpected response {other:?}"),
            });
        }
        got
    }

    /// The updates for eth0 and eth1 a client of `list` receives after the
    /// sync, in 1.1 s, while eth0 changes every 10 ms and eth1 is published
    /// again with its unchanged value, on a target with a 100 ms floor; and
    /// how many values eth0 took.
    async fn sampled_while_one_series_changes(
        list: proto::SubscriptionList,
    ) -> (usize, usize, f64) {
        let mut sink = sink(None, Some("100ms")).await;
        publish(&mut sink, "in_octets", "eth0", 0.0).await;
        publish(&mut sink, "in_octets", "eth1", 7.0).await;
        let mut client = open(sink.target().local_addr(), list)
            .await
            .expect("subscribed");
        let mut synced = vec![next_value(&mut client).await, next_value(&mut client).await];
        synced.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(
            synced,
            [
                Some((counter("eth0", "in-octets"), Value::DoubleVal(0.0))),
                Some((counter("eth1", "in-octets"), Value::DoubleVal(7.0))),
            ]
        );
        assert_eq!(next_value(&mut client).await, None, "the sync marker");

        let started = Instant::now();
        let mut received = Vec::new();
        let mut value = 0.0;
        while started.elapsed() < Duration::from_millis(1_100) {
            value += 1.0;
            publish(&mut sink, "in_octets", "eth0", value).await;
            publish(&mut sink, "in_octets", "eth1", 7.0).await;
            received.extend(
                values_until(&mut client, Instant::now() + Duration::from_millis(10)).await,
            );
        }
        assert!(!received.contains(&None), "no second sync marker");
        (
            updates_for(&received, &counter("eth0", "in-octets")),
            updates_for(&received, &counter("eth1", "in-octets")),
            value,
        )
    }

    /// SAMPLE with `suppress_redundant` sends a series whose value holds once,
    /// in the initial sync, and a changing one every period.
    #[tokio::test]
    async fn suppress_redundant_sends_an_unchanged_series_only_once() {
        let (changing, steady, changes) = sampled_while_one_series_changes(suppressed(
            "/interfaces",
            Duration::from_millis(200),
            Duration::ZERO,
        ))
        .await;
        assert!(changes > 50.0, "the premise: eth0 changed {changes} times");
        assert!(
            (4..=6).contains(&changing),
            "{changing} updates of the changing series in 1.1 s at 200 ms"
        );
        assert_eq!(steady, 0, "the unchanged series was sent only in the sync");
    }

    /// A suppressing SAMPLE subscription's heartbeat re-sends a series whose
    /// value holds, every heartbeat.
    #[tokio::test]
    async fn a_sample_heartbeat_resends_an_unchanged_series() {
        let (changing, steady, changes) = sampled_while_one_series_changes(suppressed(
            "/interfaces",
            Duration::from_millis(200),
            Duration::from_millis(300),
        ))
        .await;
        assert!(changes > 50.0, "the premise: eth0 changed {changes} times");
        assert!(changing >= 4, "{changing} updates of the changing series");
        assert!(
            (2..=4).contains(&steady),
            "{steady} heartbeats of the unchanged series in 1.1 s at 300 ms"
        );
    }

    /// The control: the same SAMPLE list without `suppress_redundant` sends
    /// both series every period, and its heartbeat interval is not read.
    #[tokio::test]
    async fn without_suppress_redundant_every_period_sends_every_series() {
        let mut list = suppressed(
            "/interfaces",
            Duration::from_millis(200),
            Duration::from_millis(300),
        );
        list.subscription[0].suppress_redundant = false;
        let (changing, steady, changes) = sampled_while_one_series_changes(list).await;
        assert!(changes > 50.0, "the premise: eth0 changed {changes} times");
        for (count, which) in [(changing, "changing"), (steady, "unchanged")] {
            assert!(
                (4..=6).contains(&count),
                "{count} updates of the {which} series in 1.1 s at 200 ms"
            );
        }
    }

    /// A target with a 100 ms floor and two sinks of one launch, one
    /// publishing eth0 and the other eth1.
    async fn two_sinks_of_one_launch() -> (Arc<GnmiTarget>, GnmiTargetSink, GnmiTargetSink) {
        let target = target_with(4, |settings| {
            settings.sample_floor = Duration::from_millis(100);
        })
        .await;
        let launch = LaunchId::next();
        let mut first = GnmiTargetSink::from_target(Arc::clone(&target), launch);
        let mut second = GnmiTargetSink::from_target(Arc::clone(&target), launch);
        publish(&mut first, "in_octets", "eth0", 1.0).await;
        publish(&mut second, "in_octets", "eth1", 1.0).await;
        assert_eq!(target.series_count(), 2, "the premise: two series");
        (target, first, second)
    }

    /// The initial sync of eth0 and eth1 at 1.0, in either order, then the
    /// marker.
    async fn assert_synced(client: &mut Client) {
        let mut synced = got_until(client, Instant::now() + Duration::from_millis(50)).await;
        let marker = synced.pop();
        synced.sort_by_key(|g| format!("{g:?}"));
        assert_eq!(
            synced,
            [
                Got::Value(counter("eth0", "in-octets"), Value::DoubleVal(1.0)),
                Got::Value(counter("eth1", "in-octets"), Value::DoubleVal(1.0)),
            ]
        );
        assert_eq!(marker, Some(Got::Sync));
    }

    /// Dropping one of two sinks of a launch removes its series: an
    /// ON_CHANGE client receives exactly one delete, for that path, and no
    /// value for it after; the other series' changes still arrive.
    #[tokio::test]
    async fn dropping_a_sink_sends_one_delete_to_an_on_change_client() {
        let (target, first, mut second) = two_sinks_of_one_launch().await;
        let mut client = open(
            target.local_addr(),
            on_change("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_synced(&mut client).await;

        drop(first);
        publish(&mut second, "in_octets", "eth1", 2.0).await;
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(300)).await,
            [
                Got::Delete(counter("eth0", "in-octets")),
                Got::Value(counter("eth1", "in-octets"), Value::DoubleVal(2.0)),
            ]
        );
        assert_eq!(target.series_count(), 1);
    }

    /// An ON_CHANGE client receives the delete of a series released just
    /// after another series' change, when reading that change prunes the
    /// set's match cache: the prune keeps the series the set matches.
    #[tokio::test]
    async fn an_on_change_client_receives_a_delete_across_a_match_cache_prune() {
        let target = target_with(4, |_| {}).await;
        let launch = LaunchId::next();
        let mut bulk = GnmiTargetSink::from_target(Arc::clone(&target), launch);
        let mut lone = GnmiTargetSink::from_target(Arc::clone(&target), launch);
        let mut extra = GnmiTargetSink::from_target(Arc::clone(&target), launch);
        for i in 0..MATCHED_PRUNE_FLOOR - 1 {
            publish(&mut bulk, "in_octets", &format!("eth{i}"), 1.0).await;
        }
        publish(&mut lone, "in_octets", "lone", 1.0).await;
        let mut client = open(
            target.local_addr(),
            on_change("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        let mut synced = 0;
        while next_value(&mut client).await.is_some() {
            synced += 1;
        }
        assert_eq!(
            synced, MATCHED_PRUNE_FLOOR,
            "the premise: the cache is full"
        );

        publish(&mut extra, "in_octets", "extra", 1.0).await;
        drop(lone);
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(300)).await,
            [
                Got::Value(counter("extra", "in-octets"), Value::DoubleVal(1.0)),
                Got::Delete(counter("lone", "in-octets")),
            ]
        );
        assert_eq!(
            target
                .settings()
                .probe
                .prunes
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the premise: the new series' change pruned the cache"
        );
    }

    /// A SAMPLE client with `suppress_redundant` receives the removed
    /// series' delete at its next sample, once; the unchanged series it is
    /// not sent again.
    #[tokio::test]
    async fn a_suppressing_sample_client_receives_the_delete_at_its_next_sample() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        let mut client = open(
            target.local_addr(),
            suppressed("/interfaces", Duration::from_millis(200), Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_synced(&mut client).await;

        drop(first);
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(700)).await,
            [Got::Delete(counter("eth0", "in-octets"))]
        );
    }

    /// A path published again by a new series before a suppressing SAMPLE
    /// client's next sample is deleted, then sent with its new value.
    #[tokio::test]
    async fn a_suppressing_sample_client_receives_a_delete_before_the_path_published_again() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        let mut client = open(
            target.local_addr(),
            suppressed("/interfaces", Duration::from_millis(200), Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_synced(&mut client).await;

        drop(first);
        let mut again = GnmiTargetSink::from_target(Arc::clone(&target), LaunchId::next());
        publish(&mut again, "in_octets", "eth0", 5.0).await;
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(700)).await,
            [
                Got::Delete(counter("eth0", "in-octets")),
                Got::Value(counter("eth0", "in-octets"), Value::DoubleVal(5.0)),
            ]
        );
    }

    /// A SAMPLE client without `suppress_redundant` sees a removed series
    /// stop appearing, and receives no delete; the other series is still
    /// sampled.
    #[tokio::test]
    async fn a_plain_sample_client_sees_a_removed_series_stop_without_a_delete() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        let mut client = open(
            target.local_addr(),
            stream("/interfaces", Duration::from_millis(200)),
        )
        .await
        .expect("subscribed");
        assert_synced(&mut client).await;
        let before = got_until(&mut client, Instant::now() + Duration::from_millis(250)).await;
        assert!(
            before.contains(&Got::Value(
                counter("eth0", "in-octets"),
                Value::DoubleVal(1.0)
            )),
            "the premise: eth0 was sampled, in {before:?}"
        );

        drop(first);
        // A sample taken before the drop may still be on its way.
        let settling = got_until(&mut client, Instant::now() + Duration::from_millis(250)).await;
        let after = got_until(&mut client, Instant::now() + Duration::from_millis(600)).await;
        let eth1 = Got::Value(counter("eth1", "in-octets"), Value::DoubleVal(1.0));
        assert!(
            settling
                .iter()
                .chain(&after)
                .all(|g| !matches!(g, Got::Delete(_))),
            "no delete: {settling:?} {after:?}"
        );
        assert!(
            after.len() >= 2 && after.iter().all(|g| *g == eth1),
            "only eth1 after the drop: {after:?}"
        );
    }

    /// A series published again on a path after its delete is a new series,
    /// and an ON_CHANGE client receives it.
    #[tokio::test]
    async fn a_series_published_again_after_its_delete_is_delivered() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        let mut client = open(
            target.local_addr(),
            on_change("/interfaces", Duration::ZERO),
        )
        .await
        .expect("subscribed");
        assert_synced(&mut client).await;
        drop(first);
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(200)).await,
            [Got::Delete(counter("eth0", "in-octets"))]
        );

        let mut again = GnmiTargetSink::from_target(Arc::clone(&target), LaunchId::next());
        publish(&mut again, "in_octets", "eth0", 5.0).await;
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(200)).await,
            [Got::Value(
                counter("eth0", "in-octets"),
                Value::DoubleVal(5.0)
            )]
        );
    }

    /// A list whose ON_CHANGE subscription and suppressing SAMPLE
    /// subscription both had the removed series sends one delete for it.
    #[tokio::test]
    async fn a_list_sends_one_delete_for_a_removed_series() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        let mut list = suppressed("/interfaces", Duration::from_millis(200), Duration::ZERO);
        list.subscription.push(
            on_change("/interfaces", Duration::ZERO)
                .subscription
                .remove(0),
        );
        let mut client = open(target.local_addr(), list).await.expect("subscribed");
        assert_synced(&mut client).await;

        drop(first);
        assert_eq!(
            got_until(&mut client, Instant::now() + Duration::from_millis(700)).await,
            [Got::Delete(counter("eth0", "in-octets"))]
        );
    }

    /// A ONCE subscription after a series was removed receives the series
    /// that remain, the marker and the end, and no delete.
    #[tokio::test]
    async fn once_after_a_removal_sends_only_the_remaining_series() {
        let (target, first, _second) = two_sinks_of_one_launch().await;
        drop(first);
        let mut client = open(target.local_addr(), once("/interfaces"))
            .await
            .expect("subscribed");
        assert_eq!(
            next(&mut client).await,
            Received::Update(counter("eth1", "in-octets"))
        );
        assert_eq!(next(&mut client).await, Received::Sync);
        assert_eq!(next(&mut client).await, Received::End);
    }

    /// A client that reads nothing while every matched series changes many
    /// times holds at most one queued value per series: a newer change
    /// replaces the queued one. When it reads, it receives the latest value
    /// of every series, and fewer updates than two per series.
    #[tokio::test]
    async fn a_slow_on_change_client_queues_at_most_one_value_per_series() {
        const ROUNDS: usize = 10;
        let target = target_with(1, |settings| {
            settings.subscriber_buffer = MAX_SUBSCRIBER_BUFFER;
        })
        .await;
        let owner = Owner::from(LaunchId::next());
        let ids: Vec<SeriesId> = (0..STALL_SERIES)
            .map(|i| publish_as(&target, owner, None, i, 0.0).0)
            .collect();
        let mut client = open_on(
            connect_with_small_window(target.local_addr()).await,
            on_change("/interfaces", Duration::ZERO),
            true,
        )
        .await
        .expect("subscribed");
        for round in 1..=ROUNDS {
            for (i, id) in ids.iter().enumerate() {
                publish_as(&target, owner, Some(*id), i, round as f64);
            }
            tokio::task::yield_now().await;
        }
        assert!(
            eventually(|| target.broadcast_backlog() == 0).await,
            "the subscription read every change while its client read nothing"
        );
        let probe = &target.settings().probe;
        let queued = probe
            .max_queued_values
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            queued <= STALL_SERIES,
            "{queued} values queued for {STALL_SERIES} series"
        );
        assert!(
            queued > STALL_SERIES / 2,
            "the premise: the queue backed up ({queued} at most)"
        );
        assert_eq!(probe.resyncs.load(std::sync::atomic::Ordering::Relaxed), 0);

        let last = Value::DoubleVal(ROUNDS as f64);
        let mut latest: HashMap<String, Value> = HashMap::new();
        let mut updates = 0usize;
        while latest.len() < STALL_SERIES || latest.values().any(|v| *v != last) {
            if let Some((path, value)) = next_value(&mut client).await {
                updates += 1;
                latest.insert(path, value);
            }
        }
        assert!(
            updates <= 2 * STALL_SERIES,
            "{updates} updates for {STALL_SERIES} series changed {ROUNDS} times each"
        );
    }

    /// In a list mixing SAMPLE and ON_CHANGE, a sample group falls due while
    /// only changes wait for a client that reads nothing, and queues one
    /// sample, not one per period.
    #[tokio::test]
    async fn a_sample_group_falls_due_while_only_changes_wait() {
        let period = Duration::from_millis(400);
        let target = target_with(1, |settings| {
            settings.sample_floor = period;
            settings.subscriber_buffer = MAX_SUBSCRIBER_BUFFER;
        })
        .await;
        let owner = Owner::from(LaunchId::next());
        let ids: Vec<SeriesId> = (0..STALL_SERIES)
            .map(|i| publish_as(&target, owner, None, i, 0.0).0)
            .collect();
        let mut list = on_change("/interfaces", Duration::ZERO);
        list.subscription.push(
            stream("/interfaces/interface[name=eth0]", Duration::ZERO)
                .subscription
                .remove(0),
        );
        // The sync marker alone goes out at once, leaving no response queued.
        list.updates_only = true;
        let opened = Instant::now();
        let mut client = open_on(
            connect_with_small_window(target.local_addr()).await,
            list,
            true,
        )
        .await
        .expect("subscribed");
        for (i, id) in ids.iter().enumerate() {
            publish_as(&target, owner, Some(*id), i, 1.0);
        }
        let probe = Arc::clone(&target.settings().probe);
        let samples = || probe.samples.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            eventually(|| target.broadcast_backlog() == 0).await,
            "the subscription read every change"
        );
        assert_eq!(
            samples(),
            0,
            "the premise: the changes were queued within {:?}, before the first sample",
            opened.elapsed()
        );
        assert!(
            probe
                .max_queued_values
                .load(std::sync::atomic::Ordering::Relaxed)
                > STALL_SERIES / 2,
            "the premise: changes are waiting"
        );
        tokio::time::sleep_until(opened + period * 5 / 2).await;
        assert_eq!(
            samples(),
            1,
            "two periods passed: one sample is queued behind the changes, and no more"
        );
        assert_eq!(next_value(&mut client).await, None, "the sync marker");
    }

    /// A client that keeps its connection open but never reads is ended at
    /// the stall floor although changes keep arriving for its ON_CHANGE
    /// list: an ON_CHANGE list has no sample periods, a heartbeat period does
    /// not lengthen the limit, and a change does not restart it.
    #[tokio::test]
    async fn an_on_change_client_that_drains_nothing_is_ended_at_the_floor() {
        let target = target_with(1, |settings| {
            settings.stall_floor = STALL_TEST_FLOOR;
            settings.subscriber_buffer = MAX_SUBSCRIBER_BUFFER;
        })
        .await;
        let owner = Owner::from(LaunchId::next());
        let ids: Vec<SeriesId> = (0..STALL_SERIES)
            .map(|i| publish_as(&target, owner, None, i, 0.0).0)
            .collect();
        let opened = Instant::now();
        let _stalled = open_on(
            connect_with_small_window(target.local_addr()).await,
            on_change("/interfaces", Duration::from_secs(1)),
            true,
        )
        .await
        .expect("subscribed");
        assert_eq!(target.subscriber_count(), 1);
        let mut value = 0.0;
        let ended = within(Duration::from_secs(2), || {
            value += 1.0;
            publish_as(&target, owner, Some(ids[0]), 0, value);
            target.subscriber_count() == 0
        })
        .await;
        assert!(ended, "still subscribed after {value} changes");
        assert!(
            opened.elapsed() < 3 * STALL_TEST_FLOOR,
            "ended after {:?}, past the {STALL_TEST_FLOOR:?} floor",
            opened.elapsed()
        );
    }

    /// TARGET_DEFINED puts a subscription in the ON_CHANGE set for string
    /// values and in a sample group for every other kind; ON_CHANGE with a
    /// heartbeat adds a heartbeat group; SAMPLE's heartbeat is not read.
    #[test]
    fn plan_groups_splits_a_list_by_mode_and_kind() {
        let floor = Duration::from_millis(100);
        let mut list = stream("/a", Duration::from_secs(2));
        list.subscription[0].mode = SubscriptionMode::TargetDefined as i32;
        list.subscription[0].heartbeat_interval = Duration::from_secs(5).as_nanos() as u64;
        let mut changes = on_change("/b", Duration::from_millis(1));
        let mut sampled = stream("/c", Duration::ZERO);
        sampled.subscription[0].heartbeat_interval = Duration::from_secs(5).as_nanos() as u64;
        list.subscription.push(changes.subscription.remove(0));
        list.subscription.push(sampled.subscription.remove(0));

        let plan = plan_groups(&list, floor).expect("a valid list");
        let timed = |groups: &[Group]| -> Vec<(Duration, KindFilter)> {
            groups
                .iter()
                .map(|g| (g.period, g.selector.kinds))
                .collect()
        };
        assert_eq!(
            timed(&plan.samples),
            [
                (Duration::from_secs(2), KindFilter::NonStrings),
                (floor, KindFilter::Any)
            ]
        );
        assert_eq!(
            timed(&plan.heartbeats),
            [
                (Duration::from_secs(5), KindFilter::Strings),
                (floor, KindFilter::Any)
            ]
        );
        let kinds: Vec<KindFilter> = plan.on_change.iter().map(|s| s.kinds).collect();
        assert_eq!(kinds, [KindFilter::Strings, KindFilter::Any]);
        assert_eq!(
            stall_limit(plan.samples.iter().map(|g| g.period), floor),
            Duration::from_secs(6)
        );
    }

    /// The match cache forgets series that left the table, and may then
    /// grow to twice what is left, or the floor, before the next prune.
    #[test]
    fn the_match_cache_is_pruned_to_the_series_in_the_table() {
        let (_tx, rx) = broadcast::channel(1);
        let mut set = OnChangeSet {
            selectors: Vec::new(),
            rx,
            matched: HashMap::new(),
            prune_at: MATCHED_PRUNE_FLOOR,
        };
        for id in 0..5_000 {
            set.seen(id, ValueKind::Uint, || None);
        }
        assert_eq!(set.matched.len(), 5_000, "the premise: every id was cached");
        let live: Vec<SeriesSnapshot> = (4_990..5_000)
            .map(|series_id| SeriesSnapshot {
                series_id,
                path: PathKey::new(proto::Path::default()),
                kind: ValueKind::Uint,
                version: 1,
                latest: Bytes::new(),
            })
            .collect();
        set.prune(&live);
        let mut kept: Vec<SeriesId> = set.matched.keys().copied().collect();
        kept.sort_unstable();
        assert_eq!(kept, (4_990..5_000).collect::<Vec<_>>());
        assert_eq!(set.prune_at, MATCHED_PRUNE_FLOOR);
    }

    /// The prune run on reading a change forgets only the series that left
    /// the table and that the set does not match; it keeps every series in
    /// the table, and every matched one. The cache may then grow to twice
    /// what is left.
    #[test]
    fn the_prune_on_a_change_keeps_matched_series_that_left_the_table() {
        let (_tx, rx) = broadcast::channel(1);
        let mut set = OnChangeSet {
            selectors: Vec::new(),
            rx,
            matched: HashMap::new(),
            prune_at: MATCHED_PRUNE_FLOOR,
        };
        let seen = |matches| Seen { matches, queued: 0 };
        for id in 0..3_000 {
            set.matched.insert(id, seen(false));
        }
        for id in 3_000..3_600 {
            set.matched.insert(id, seen(true));
        }
        // 10 unmatched and 10 matched series are still in the table.
        let live: Vec<SeriesSnapshot> = (2_990..3_010)
            .map(|series_id| SeriesSnapshot {
                series_id,
                path: PathKey::new(proto::Path::default()),
                kind: ValueKind::Uint,
                version: 1,
                latest: Bytes::new(),
            })
            .collect();
        set.prune_unmatched(&live);
        let mut kept: Vec<SeriesId> = set.matched.keys().copied().collect();
        kept.sort_unstable();
        assert_eq!(kept, (2_990..3_600).collect::<Vec<_>>());
        assert!(
            2 * kept.len() > MATCHED_PRUNE_FLOOR,
            "the premise: twice what is left is above the floor"
        );
        assert_eq!(set.prune_at, 2 * kept.len());
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
