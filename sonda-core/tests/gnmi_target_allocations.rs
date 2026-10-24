//! Allocation surcharge of the `gnmi_target` sink over a plain sink, through
//! the real runner, and over 10 000 writes made directly.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter is thread-local and each run uses a current-thread
//! runtime, so the runner, the sink and the target's listener task all
//! allocate on the measuring thread, and nothing on any other thread is
//! counted.
//!
//! Both runs use the same two-label scenario and the same `gnmi` encoder, so
//! the difference between them is what the sink adds per event.
//!
//! The subscriber cases host the target on a multi-thread runtime of their
//! own, so its listener, its subscriptions and their clients run on other
//! threads; the publisher, on the measuring thread, is all that is counted.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(all(feature = "gnmi", feature = "config"))]

mod common;
// `common` is compiled only with `config`; the counter is needed without it.
#[path = "common/alloc.rs"]
mod counting;

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Buf;
use prost::Message;
use tokio_util::sync::CancellationToken;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder as TonicEncoder};
use tonic::Status;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::gnmi::proto::{
    self, subscribe_request, subscribe_response, subscription_list, typed_value, Encoding,
    SubscriptionMode,
};
use sonda_core::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
use sonda_core::encoder::{Encoder, EncoderConfig};
use sonda_core::generator::GeneratorConfig;
use sonda_core::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::gnmi::{GnmiTarget, GnmiTargetConfig, GnmiTargetSink};
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{OnSinkError, SondaError};

#[global_allocator]
static ALLOCATOR: counting::CountingAlloc = counting::CountingAlloc;

use counting::count_allocations;

/// Sink that does not opt in, so it keeps the plain byte path.
struct PlainSink {
    delivered: Arc<AtomicUsize>,
}

#[async_trait]
impl Sink for PlainSink {
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn scenario() -> ScenarioConfig {
    scenario_of(GeneratorConfig::Constant { value: 1.0 })
}

/// The two-label scenario with `generator`.
fn scenario_of(generator: GeneratorConfig) -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name: "alloc_probe".to_string(),
            rate: 1000.0,
            duration: Some("1s".to_string()),
            gaps: None,
            gap_windows: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            // Two labels, so a deep clone of the label set would cost
            // allocations and show up in the surcharge.
            labels: Some(std::collections::HashMap::from([
                ("device".to_string(), "leaf-1".to_string()),
                ("site".to_string(), "ams".to_string()),
            ])),
            sink: SinkConfig::GnmiTarget(GnmiTargetConfig::new("127.0.0.1:0")),
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        },
        generator,
        // `device` is the default target label; `site` is in the path.
        encoder: EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some("/sites/site[name={site}]/{name}".to_string()),
            ..GnmiEncoderConfig::default()
        }),
        metric_type: None,
        help: None,
    }
}

/// Drive the real runner for one second at 1 kHz on `rt` and return
/// allocations per event, having first warmed the same sink outside the
/// counted window so first-sight state (the encoder's path cache, the
/// target's series slot) is not charged to the loop.
fn allocations_per_event(rt: &tokio::runtime::Runtime, sink: &mut Box<dyn Sink>) -> (f64, u64) {
    allocations_per_event_of(rt, sink, &scenario())
}

/// [`allocations_per_event`] for `config`.
fn allocations_per_event_of(
    rt: &tokio::runtime::Runtime,
    sink: &mut Box<dyn Sink>,
    config: &ScenarioConfig,
) -> (f64, u64) {
    let (per_event, events, _) = measure(rt, sink, config);
    (per_event, events)
}

/// [`allocations_per_event_of`], and the value of the last event of the
/// counted run.
fn measure(
    rt: &tokio::runtime::Runtime,
    sink: &mut Box<dyn Sink>,
    config: &ScenarioConfig,
) -> (f64, u64, f64) {
    let config = config.clone();
    let cancel = CancellationToken::new();

    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
            .await
            .expect("warm-up run must succeed");
    });

    let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        rt.block_on(async {
            sonda_core::schedule::runner::run_with_sink(
                &config,
                sink,
                &cancel,
                Some(Arc::clone(&stats)),
            )
            .await
            .expect("counted run must succeed");
        })
    });
    let stats = stats.read().expect("stats lock");
    let events = stats.total_events;
    // Vacuity guard: a run that emitted nothing would divide into a flattering
    // zero, so assert the events exist before asserting the ratio.
    assert!(
        events >= 500,
        "a 1 s run at 1 kHz must emit ~1000 events before the ratio means \
         anything; got {events}"
    );
    let last = stats
        .current_values
        .values()
        .map(|event| event.value)
        .next()
        .expect("the run emitted a series");
    (allocs as f64 / events as f64, events, last)
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime must build")
}

/// One known allocation on this thread reads as one: this binary's counting
/// allocator is installed.
#[test]
fn a_known_allocation_counts_as_one() {
    counting::assert_count_allocations_counts();
}

/// Another thread allocating throughout a counted window adds nothing to it,
/// so this binary's cases can run in parallel.
#[test]
fn allocations_on_other_threads_are_not_counted() {
    const OTHER_ALLOCS: usize = 10_000;
    let go = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let other = {
        let (go, done) = (Arc::clone(&go), Arc::clone(&done));
        std::thread::spawn(move || {
            while !go.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            for i in 0..OTHER_ALLOCS {
                std::hint::black_box(vec![i as u8; 16]);
                done.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        go.store(true, Ordering::SeqCst);
        while done.load(Ordering::SeqCst) < OTHER_ALLOCS {
            std::hint::spin_loop();
        }
        std::hint::black_box(Box::new(1u8));
    });
    other.join().expect("allocating thread panicked");
    // Vacuity guard: the other thread did allocate, all of it inside the window.
    assert_eq!(done.load(Ordering::SeqCst), OTHER_ALLOCS);
    assert_eq!(
        allocs, 1,
        "only this thread's own allocation is counted, none of the other thread's"
    );
}

/// Through the runner, the `gnmi_target` sink costs less than half an
/// allocation per event more than a plain sink, with no subscriber
/// connected: each notification is copied into a buffer the sink reuses.
#[test]
fn gnmi_target_surcharge_over_a_plain_sink_is_under_one_half() {
    let rt = current_thread_runtime();

    let delivered = Arc::new(AtomicUsize::new(0));
    let mut plain: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let (plain_cost, plain_events) = allocations_per_event(&rt, &mut plain);
    assert!(
        delivered.load(Ordering::Relaxed) as u64 >= plain_events,
        "the plain sink received every event through write"
    );
    assert!(plain_cost > 0.0, "the plain sink's figure was counted");

    let target_sink = rt
        .block_on(GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    let target: Arc<GnmiTarget> = Arc::clone(target_sink.target());
    let mut sink: Box<dyn Sink> = Box::new(target_sink);
    let (target_cost, _) = allocations_per_event(&rt, &mut sink);

    // Vacuity guards: the events reached the target as one series, and
    // nobody was subscribed to cost a broadcast clone.
    assert_eq!(target.series_count(), 1, "the target holds the series");
    assert_eq!(target.subscriber_count(), 0);

    let surcharge = target_cost - plain_cost;
    println!(
        "allocations per event: plain {plain_cost:.3}, gnmi_target {target_cost:.3}, \
         surcharge {surcharge:.3}"
    );
    assert!(
        surcharge < 0.5,
        "the gnmi_target sink is over budget: {target_cost:.3} allocations per \
         event against {plain_cost:.3} for a plain sink, a surcharge of \
         {surcharge:.3} (allowed < 0.5)"
    );
    drop(sink);
    drop(target);
}

/// Publishes [`publish_cost`] counts, after its warm-up.
const PUBLISHES: usize = 10_000;

/// The allocations per event of writing `events` in turn, `PUBLISHES` times
/// in all, after a warm-up of two writes per event: through
/// [`Sink::write_metric_event`] when the sink takes metric events, else
/// through [`Sink::write`], as the runner calls them. Each event is encoded
/// into one reused buffer before its write.
fn publish_cost(
    rt: &tokio::runtime::Runtime,
    sink: &mut Box<dyn Sink>,
    events: &[MetricEvent],
) -> f64 {
    let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
        path: Some("/sites/site[name={site}]/{name}".to_string()),
        ..GnmiEncoderConfig::default()
    })
    .expect("a valid encoder");
    let mut buf = Vec::with_capacity(4096);
    let mut write = |sink: &mut Box<dyn Sink>, event: &MetricEvent| {
        buf.clear();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        rt.block_on(async {
            if sink.wants_metric_events() {
                sink.write_metric_event(event, &buf).await
            } else {
                sink.write(&buf).await
            }
        })
        .expect("the write succeeds");
    };
    for event in events.iter().chain(events) {
        write(sink, event);
    }
    let mut written = 0usize;
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        for event in events.iter().cycle().take(PUBLISHES) {
            write(sink, event);
            written += 1;
        }
    });
    // Vacuity guard: every counted write ran.
    assert_eq!(written, PUBLISHES);
    allocs as f64 / written as f64
}

/// `series` events on distinct sites, built before any counted window.
fn site_events(series: usize) -> Vec<MetricEvent> {
    (0..series)
        .map(|i| {
            let site = format!("site-{i}");
            MetricEvent::from_parts(
                ValidatedMetricName::new("alloc_probe").expect("a valid name"),
                1.0,
                Arc::new(
                    Labels::from_pairs(&[("device", "leaf-1"), ("site", &site)])
                        .expect("valid labels"),
                ),
                std::time::SystemTime::now(),
            )
        })
        .collect()
}

/// Over 10 000 publishes after a warm-up of two per series, with nobody
/// subscribed, the `gnmi_target` sink allocates per event what a plain sink
/// does: each path's notification is copied into one of two buffers the
/// sink keeps for that path, whose last `Bytes` the table has dropped. That
/// holds for a sink publishing one series and for one publishing two in
/// turn.
#[rstest::rstest]
#[case::one_series(1)]
#[case::two_series(2)]
fn with_nobody_subscribed_the_gnmi_target_surcharge_is_zero(#[case] series: usize) {
    let rt = current_thread_runtime();
    let events = site_events(series);

    let delivered = Arc::new(AtomicUsize::new(0));
    let mut plain: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let plain_cost = publish_cost(&rt, &mut plain, &events);
    assert!(delivered.load(Ordering::Relaxed) >= PUBLISHES);
    assert!(plain_cost > 0.0, "the plain sink's figure was counted");

    let target_sink = rt
        .block_on(GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    let target: Arc<GnmiTarget> = Arc::clone(target_sink.target());
    let mut sink: Box<dyn Sink> = Box::new(target_sink);
    let target_cost = publish_cost(&rt, &mut sink, &events);

    // Vacuity guards: every event reached the target as its own series,
    // and nobody was subscribed.
    assert_eq!(target.series_count(), series);
    assert_eq!(target.subscriber_count(), 0);

    let surcharge = target_cost - plain_cost;
    println!(
        "allocations per publish, {series} series, nobody subscribed: plain {plain_cost:.3}, \
         gnmi_target {target_cost:.3}, surcharge {surcharge:.3}"
    );
    assert!(
        surcharge < 0.01,
        "with nobody subscribed the gnmi_target sink allocates {surcharge:.3} per publish \
         more than a plain sink (allowed < 0.01)"
    );
    drop(sink);
}

/// With a value that changes on every event and one ON_CHANGE subscriber
/// reading every change, in PROTO or in JSON_IETF, the `gnmi_target` sink
/// costs less than 1.5 allocations per event more than a plain sink. A
/// publish allocates only when the subscriber still holds the `Bytes` last
/// frozen from the buffer it reuses, so the figure depends on how fast the
/// subscriber drains and is reported.
#[rstest::rstest]
#[case::proto(Encoding::Proto)]
#[case::json_ietf(Encoding::JsonIetf)]
fn a_draining_subscriber_costs_the_publisher_under_one_and_a_half(#[case] encoding: Encoding) {
    let rt = current_thread_runtime();
    let other = other_threads_runtime();
    let config = scenario_of(GeneratorConfig::Step {
        start: None,
        step_size: 1.0,
        max: None,
    });

    let delivered = Arc::new(AtomicUsize::new(0));
    let mut plain: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let (plain_cost, _) = allocations_per_event_of(&rt, &mut plain, &config);
    assert!(plain_cost > 0.0, "the plain sink's figure was counted");

    let (mut sink, target) = sink_on(&other);
    let received = subscribe(&other, &target, encoding);
    let (with_subscriber, events) = allocations_per_event_of(&rt, &mut sink, &config);

    // Vacuity guards: the subscriber stayed and was sent the changes in its
    // encoding.
    assert_eq!(target.subscriber_count(), 1, "the subscriber stayed");
    let values = received.values.load(Ordering::Relaxed) as u64;
    assert!(
        values >= events / 2,
        "the subscriber received {values} values for {events} changes"
    );
    assert_eq!(received.foreign.load(Ordering::Relaxed), 0);

    let surcharge = with_subscriber - plain_cost;
    println!(
        "allocations per event, value changing, one draining {encoding:?} subscriber: plain \
         {plain_cost:.3}, gnmi_target {with_subscriber:.3}, surcharge {surcharge:.3}"
    );
    assert!(
        surcharge < 1.5,
        "a draining {encoding:?} subscriber costs the publisher {surcharge:.3} allocations per \
         event over a plain sink (allowed < 1.5)"
    );
    drop(sink);
}

/// Dropping a sink releases its series. With nobody subscribed, the release
/// allocates nothing: no delete is encoded. The target is held, so dropping
/// the sink does not stop it.
#[test]
fn a_release_with_no_receiver_allocates_nothing() {
    let rt = current_thread_runtime();
    let mut sink = rt
        .block_on(GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    let target: Arc<GnmiTarget> = Arc::clone(sink.target());
    let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
        path: Some("/sites/site[name={site}]/{name}".to_string()),
        ..GnmiEncoderConfig::default()
    })
    .expect("valid encoder");
    for site in ["ams", "fra", "lon"] {
        let event = MetricEvent::new(
            "up".to_string(),
            1.0,
            Labels::from_pairs(&[("site", site)]).expect("valid labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        rt.block_on(sink.write_metric_event(&event, &buf))
            .expect("published");
    }
    // Vacuity guards: the series are in the table and nobody is subscribed.
    assert_eq!(target.series_count(), 3);
    assert_eq!(target.subscriber_count(), 0);

    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| drop(sink));

    assert_eq!(target.series_count(), 0, "the drop released every series");
    assert_eq!(
        allocs, 0,
        "releasing with no receiver allocated {allocs} times"
    );
    drop(target);
}

/// A tonic codec that sends `T` and receives `U`, encoded with prost.
struct ProstCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for ProstCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for ProstCodec<T, U>
where
    T: Message + Send + 'static,
    U: Message + Default + Send + 'static,
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

impl<T: Message> TonicEncoder for ProstEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|e| Status::internal(e.to_string()))
    }
}

struct ProstDecoder<U>(PhantomData<U>);

impl<U: Message + Default> Decoder for ProstDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        U::decode(src.copy_to_bytes(src.remaining()))
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
    }
}

/// A runtime whose worker threads host a target, its subscriptions and their
/// clients, off the measuring thread.
fn other_threads_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime must build")
}

/// A `gnmi_target` sink on `127.0.0.1:0` with a 100 ms sample floor, its
/// target bound on `other`, and the target.
fn sink_on(other: &tokio::runtime::Runtime) -> (Box<dyn Sink>, Arc<GnmiTarget>) {
    let mut config = GnmiTargetConfig::new("127.0.0.1:0");
    config.sample_floor = Some("100ms".to_string());
    let sink = other
        .block_on(GnmiTargetSink::new(&config))
        .expect("bind an ephemeral port");
    let target = Arc::clone(sink.target());
    (Box::new(sink), target)
}

/// What one subscriber received: values in its encoding, and values in any
/// other form.
#[derive(Default)]
struct Received {
    values: AtomicUsize,
    foreign: AtomicUsize,
}

/// Subscribe on `other` to every series under `/sites` of the target at
/// `addr`: ON_CHANGE with a heartbeat every 100 ms, in `encoding`, reading
/// every response until the stream ends. Returns once `target` counts the
/// subscription.
fn subscribe(
    other: &tokio::runtime::Runtime,
    target: &GnmiTarget,
    encoding: Encoding,
) -> Arc<Received> {
    let received = Arc::new(Received::default());
    let addr: SocketAddr = target.local_addr();
    let list = proto::SubscriptionList {
        prefix: None,
        subscription: vec![proto::Subscription {
            path: Some(proto::Path {
                elem: vec![proto::PathElem {
                    name: "sites".to_string(),
                    key: Default::default(),
                }],
                ..Default::default()
            }),
            mode: SubscriptionMode::OnChange as i32,
            heartbeat_interval: Duration::from_millis(100).as_nanos() as u64,
            ..Default::default()
        }],
        mode: subscription_list::Mode::Stream as i32,
        encoding: encoding as i32,
        updates_only: false,
    };
    let counts = Arc::clone(&received);
    other.spawn(async move {
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("a valid uri")
            .connect()
            .await
            .expect("connect to the target");
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.expect("the channel is ready");
        let request = proto::SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(list)),
        };
        let mut responses = grpc
            .streaming(
                tonic::Request::new(tokio_stream::iter([request])),
                http::uri::PathAndQuery::from_static("/gnmi.gNMI/Subscribe"),
                ProstCodec::<proto::SubscribeRequest, proto::SubscribeResponse>::default(),
            )
            .await
            .expect("subscribed")
            .into_inner();
        while let Ok(Some(response)) = responses.message().await {
            let Some(subscribe_response::Response::Update(notification)) = response.response else {
                continue;
            };
            for update in notification.update {
                let in_encoding = match update.val.and_then(|v| v.value) {
                    Some(typed_value::Value::JsonIetfVal(_)) => encoding == Encoding::JsonIetf,
                    Some(_) => encoding == Encoding::Proto,
                    None => continue,
                };
                let count = if in_encoding {
                    &counts.values
                } else {
                    &counts.foreign
                };
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while target.subscriber_count() == 0 {
        assert!(
            Instant::now() < deadline,
            "the subscription was not admitted"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    received
}

/// With the value held, a JSON_IETF subscriber does not move the publish
/// figure: the `gnmi_target` sink allocates per event what it does with
/// nobody subscribed, while the subscriber, on other threads, is served
/// heartbeats in JSON_IETF.
#[test]
fn a_json_ietf_subscriber_does_not_move_the_publish_figure() {
    let rt = current_thread_runtime();
    let other = other_threads_runtime();
    let config = scenario();

    let (mut sink, alone_target) = sink_on(&other);
    let (alone, _) = allocations_per_event_of(&rt, &mut sink, &config);
    assert_eq!(alone_target.subscriber_count(), 0);
    assert!(alone > 0.0, "the figure with nobody subscribed was counted");
    drop(sink);

    let (mut sink, target) = sink_on(&other);
    let received = subscribe(&other, &target, Encoding::JsonIetf);
    let (with_json, _) = allocations_per_event_of(&rt, &mut sink, &config);

    // Vacuity guards: the subscriber was served in JSON_IETF throughout.
    assert_eq!(target.subscriber_count(), 1, "the subscriber stayed");
    let values = received.values.load(Ordering::Relaxed);
    assert!(
        values >= 10,
        "{values} JSON_IETF values in two seconds of heartbeats"
    );
    assert_eq!(received.foreign.load(Ordering::Relaxed), 0);

    println!(
        "allocations per event, value held: nobody subscribed {alone:.3}, a JSON_IETF \
         subscriber {with_json:.3}"
    );
    assert!(
        (with_json - alone).abs() < 0.05,
        "a JSON_IETF subscriber moved the publish figure from {alone:.3} to {with_json:.3}"
    );
    drop(sink);
}

/// With nobody subscribed, a value that changes on every event costs the
/// publisher what a held value does: a change makes no JSON_IETF value, nor
/// anything else, until a subscriber asks for it.
#[test]
fn with_nobody_subscribed_a_change_costs_what_a_held_value_does() {
    let rt = current_thread_runtime();
    let other = other_threads_runtime();
    let changing = scenario_of(GeneratorConfig::Step {
        start: None,
        step_size: 1.0,
        max: None,
    });

    let (mut sink, held_target) = sink_on(&other);
    let (held, _, held_last) = measure(&rt, &mut sink, &scenario());
    drop(sink);
    let (mut sink, changing_target) = sink_on(&other);
    let (per_change, events, changing_last) = measure(&rt, &mut sink, &changing);
    drop(sink);

    // Vacuity guards: nobody was subscribed, and one value held while the
    // other changed on every event.
    assert_eq!(held_target.subscriber_count(), 0);
    assert_eq!(changing_target.subscriber_count(), 0);
    assert_eq!(held_last, 1.0, "the held value");
    assert!(held > 0.0, "the held value's figure was counted");
    assert!(
        changing_last >= (events / 2) as f64,
        "the changing value reached {changing_last} in {events} events"
    );

    println!(
        "allocations per event, nobody subscribed: value held {held:.3}, value \
         changing {per_change:.3}"
    );
    assert!(
        (per_change - held).abs() < 0.05,
        "a change costs the publisher {per_change:.3} allocations per event against \
         {held:.3} for a held value"
    );
}
