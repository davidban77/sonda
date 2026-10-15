//! gNMI target sink: serves the metric events it receives to gNMI clients.
//!
//! A [`GnmiTargetSink`] publishes every event into a [`GnmiTarget`], the
//! latest-value table and listener for one listen address. Sinks that name
//! the same address share one target through the process-wide
//! [`registry`](registry::registry), so their series appear as one device.
//!
//! The sink takes events through [`Sink::write_metric_event`] and never awaits
//! a client: publishing is a table upsert and a non-blocking broadcast send.
//!
//! Requires the `gnmi` feature flag.

pub mod registry;
pub mod service;
pub mod target;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::validate::{parse_duration, prepend_context};
use crate::model::metric::{MetricEvent, MetricKey};
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

use crate::schedule::LaunchId;
pub use registry::{registry, GnmiTargetRegistry};
pub use target::{GnmiTarget, SeriesId, SeriesUpdate};
use target::{Owner, PathKey};

/// `max_subscribers` when it is not set.
pub const DEFAULT_MAX_SUBSCRIBERS: usize = 16;

/// Broadcast capacity per target when `subscriber_buffer` is not set.
pub const DEFAULT_SUBSCRIBER_BUFFER: usize = 1024;

/// Largest accepted `subscriber_buffer`. The broadcast ring is allocated up
/// front at this many slots.
pub const MAX_SUBSCRIBER_BUFFER: usize = 65_536;

/// `sample_floor` when it is not set.
pub const DEFAULT_SAMPLE_FLOOR: Duration = Duration::from_secs(1);

/// Configuration for [`GnmiTargetSink`]; the body of
/// `sink: { type: gnmi_target, ... }`.
///
/// Unknown keys are rejected, so a misspelt field such as `listen_addr:` is an
/// error rather than a silent fallback to the defaults.
///
/// Histogram and summary entries emit `_sum` and `_count` series that carry
/// no `le` or `quantile` label. The encoder renders each series from its own
/// template, so when the bucket or quantile template references `{le}` or
/// `{quantile}`, those two series need `paths:` entries of their own
/// (`<name>_sum`, `<name>_count`); validation names any that are missing.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GnmiTargetConfig {
    /// Address to listen on, e.g. `"0.0.0.0:57400"`. `"127.0.0.1:0"` binds an
    /// ephemeral port, readable from [`GnmiTarget::local_addr`].
    #[cfg_attr(
        feature = "config",
        serde(deserialize_with = "crate::config::scalar_text::required")
    )]
    pub listen: String,
    /// Subscription stream limit stored with the target, at least 1. Default
    /// 16. Every sink on one address must use the same value.
    #[cfg_attr(feature = "config", serde(default))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub max_subscribers: Option<usize>,
    /// Broadcast capacity per target, 1 to 65536. Default 1024. Every sink
    /// on one address must use the same value.
    #[cfg_attr(feature = "config", serde(default))]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = MAX_SUBSCRIBER_BUFFER)))]
    pub subscriber_buffer: Option<usize>,
    /// Sample-interval floor stored with the target, a duration of at least
    /// one microsecond such as `"1s"`. Default `"1s"`. Every sink on one
    /// address must use the same value.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "crate::config::scalar_text::optional")
    )]
    pub sample_floor: Option<String>,
}

impl GnmiTargetConfig {
    /// A config listening on `listen` with every other field at its default.
    pub fn new(listen: impl Into<String>) -> Self {
        Self {
            listen: listen.into(),
            max_subscribers: None,
            subscriber_buffer: None,
            sample_floor: None,
        }
    }

    /// Resolve the config into the settings a target runs with.
    ///
    /// This is the one place the fields are checked; validation and
    /// [`GnmiTargetSink::new`] both call it. Returns [`SondaError::Config`]
    /// when `listen` is not a socket address, `sample_floor` is not a
    /// duration of at least one microsecond, `max_subscribers` is 0, or
    /// `subscriber_buffer` is outside 1 to [`MAX_SUBSCRIBER_BUFFER`].
    pub(crate) fn settings(&self) -> Result<TargetSettings, SondaError> {
        let invalid = |msg: String| SondaError::Config(ConfigError::invalid(msg));
        let listen: SocketAddr = self.listen.parse().map_err(|_| {
            invalid(format!(
                "gnmi_target listen {:?} is not a socket address such as \"0.0.0.0:57400\"",
                self.listen
            ))
        })?;
        let max_subscribers = self.max_subscribers.unwrap_or(DEFAULT_MAX_SUBSCRIBERS);
        if max_subscribers == 0 {
            return Err(invalid(
                "gnmi_target max_subscribers must be at least 1".to_string(),
            ));
        }
        let subscriber_buffer = self.subscriber_buffer.unwrap_or(DEFAULT_SUBSCRIBER_BUFFER);
        if !(1..=MAX_SUBSCRIBER_BUFFER).contains(&subscriber_buffer) {
            return Err(invalid(format!(
                "gnmi_target subscriber_buffer must be between 1 and {MAX_SUBSCRIBER_BUFFER}, \
                 got {subscriber_buffer}"
            )));
        }
        let sample_floor = match self.sample_floor.as_deref() {
            None => DEFAULT_SAMPLE_FLOOR,
            Some(s) => {
                let floor = parse_duration(s)
                    .map_err(|e| prepend_context("invalid gnmi_target sample_floor", s, e))?;
                // parse_duration keeps whole microseconds, so a positive value
                // below one parses as zero.
                if floor.is_zero() {
                    return Err(invalid(format!(
                        "invalid gnmi_target sample_floor {s:?}: must be at least 1us"
                    )));
                }
                floor
            }
        };
        Ok(TargetSettings {
            listen,
            max_subscribers,
            subscriber_buffer,
            sample_floor,
        })
    }
}

/// A [`GnmiTargetConfig`] with every field resolved and checked.
#[derive(Debug, Clone)]
pub(crate) struct TargetSettings {
    /// The address to bind.
    pub(crate) listen: SocketAddr,
    pub(crate) max_subscribers: usize,
    pub(crate) subscriber_buffer: usize,
    pub(crate) sample_floor: Duration,
}

/// Whether listeners on `a` and `b` would contend for one port: the same
/// address, or the same port with one of them a wildcard that covers the
/// other's family. An IPv4 wildcard covers IPv4 addresses; an IPv6 wildcard
/// covers both families. Port 0 never contends.
///
/// Treating `[::]` as covering IPv4 assumes a dual-stack socket, the Linux
/// default (`net.ipv6.bindv6only = 0`). Where it binds IPv6 only, an IPv4
/// address on the same port is refused although it could bind, and a join
/// of an IPv4 address finds the IPv6 target.
pub(crate) fn listen_overlap(a: SocketAddr, b: SocketAddr) -> bool {
    if a.port() == 0 || a.port() != b.port() {
        return false;
    }
    a == b || covers(a, b) || covers(b, a)
}

/// Whether `wild` is a wildcard listening on `other`'s port that accepts
/// `other`'s family, one way: `0.0.0.0` covers IPv4 addresses, `[::]` covers
/// both families (see [`listen_overlap`] for the dual-stack assumption).
pub(crate) fn covers(wild: SocketAddr, other: SocketAddr) -> bool {
    wild.port() != 0
        && wild.port() == other.port()
        && wild.ip().is_unspecified()
        && (wild.is_ipv6() || other.is_ipv4())
}

impl TargetSettings {
    /// Whether a target running with `self` can also serve `other`: every
    /// setting but the listen address is equal.
    pub(crate) fn serves(&self, other: &TargetSettings) -> bool {
        self.max_subscribers == other.max_subscribers
            && self.subscriber_buffer == other.subscriber_buffer
            && self.sample_floor == other.sample_floor
    }

    /// `self`'s settings beside `other`'s, for an error saying they differ:
    /// `max_subscribers 16 and 4, subscriber_buffer 1024 and 1024, sample_floor
    /// 1s and 1s`. Both address-sharing checks word the conflict with it.
    pub(crate) fn beside(&self, other: &TargetSettings) -> String {
        format!(
            "max_subscribers {} and {}, subscriber_buffer {} and {}, sample_floor {:?} and {:?}",
            self.max_subscribers,
            other.max_subscribers,
            self.subscriber_buffer,
            other.subscriber_buffer,
            self.sample_floor,
            other.sample_floor,
        )
    }
}

/// Publishes metric events into the shared [`GnmiTarget`] for its listen address.
///
/// Only [`Sink::write_metric_event`] carries data: the sink needs the event's
/// series identity, so it opts in through [`Sink::wants_metric_events`] and
/// [`Sink::write`] is an error. The first event of each series decodes the
/// series' path from the encoded notification; later events of that series
/// reuse the id the target returned. A series belongs to the launch whose
/// sink created it: the sinks of that launch share it, a sink of another
/// launch is refused it, and it leaves the target when the launch's last
/// sink drops. Dropping the last sink on an address stops its listener.
///
/// A sink made by joining a running target publishes no series of its own:
/// it replaces the latest value of a series the target already serves, and
/// refuses an event for any other path.
pub struct GnmiTargetSink {
    target: Arc<GnmiTarget>,
    owner: Owner,
    owned: HashMap<MetricKey, (SeriesId, PathKey)>,
    last_delivered: bool,
    joined: bool,
}

impl GnmiTargetSink {
    /// Join the target for `cfg.listen`, binding it if no live target has
    /// that address, as a launch of its own.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or a live target on the
    /// same address with different settings; [`SondaError::Sink`] when the
    /// address cannot be bound.
    pub async fn new(cfg: &GnmiTargetConfig) -> Result<Self, SondaError> {
        Self::new_in(cfg, LaunchId::next()).await
    }

    /// [`new`](Self::new) for a scenario of `launch`.
    pub(crate) async fn new_in(
        cfg: &GnmiTargetConfig,
        launch: LaunchId,
    ) -> Result<Self, SondaError> {
        Ok(Self::from_target(registry().acquire(cfg).await?, launch))
    }

    /// Join the live target that serves `cfg.listen` without binding. The
    /// sink only replaces the latest value of series the target already
    /// serves; it owns none.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] when no live target serves the address, or a
    /// setting `cfg` sets differs from the target's.
    pub(crate) fn join(cfg: &GnmiTargetConfig) -> Result<Self, SondaError> {
        Ok(Self {
            owner: Owner::from(LaunchId::next()),
            target: registry().join(cfg)?,
            owned: HashMap::new(),
            last_delivered: false,
            joined: true,
        })
    }

    /// A sink of `launch` publishing into `target`, counted among the
    /// launch's live sinks until it drops.
    pub(crate) fn from_target(target: Arc<GnmiTarget>, launch: LaunchId) -> Self {
        let owner = Owner::from(launch);
        target.open_sink(owner);
        Self {
            owner,
            target,
            owned: HashMap::new(),
            last_delivered: false,
            joined: false,
        }
    }

    /// The target this sink publishes into.
    pub fn target(&self) -> &Arc<GnmiTarget> {
        &self.target
    }
}

#[async_trait]
impl Sink for GnmiTargetSink {
    /// Always an error: this sink needs the event, which only
    /// [`Sink::write_metric_event`] carries.
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        Err(SondaError::Sink(std::io::Error::other(
            "the gnmi_target sink takes metric events from the gnmi encoder, not raw \
             bytes; pair it with `encoder: { type: gnmi }` on a metrics, histogram or \
             summary entry",
        )))
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }

    /// `true` once a write has upserted its event into the target.
    fn last_write_delivered(&self) -> bool {
        self.last_delivered
    }

    fn wants_metric_events(&self) -> bool {
        true
    }

    /// Publish `encoded` as the latest value of the event's series. Never
    /// awaits.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when the target's listener task has exited, or
    /// when the first event of a series is not a gnmi notification;
    /// [`SondaError::Config`] when the event's path belongs to another launch,
    /// or a joined sink's event names a path the target does not serve.
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        self.last_delivered = false;
        self.target.check_serving()?;
        if self.joined {
            let path = PathKey::decode(encoded)?;
            if self.target.overwrite(&path, encoded).is_none() {
                return Err(SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario serves the gnmi path `{}` on {}",
                    path.path(),
                    self.target.local_addr()
                ))));
            }
            self.last_delivered = true;
            return Ok(());
        }
        let key = (event.name.clone(), Arc::clone(&event.labels));
        match self.owned.get_mut(&key) {
            Some((id, path)) => {
                let (published, _) = self.target.publish(self.owner, Some(*id), path, encoded)?;
                *id = published;
            }
            None => {
                let path = PathKey::decode(encoded)?;
                let (id, _) = self.target.publish(self.owner, None, &path, encoded)?;
                self.owned.insert(key, (id, path));
            }
        }
        self.last_delivered = true;
        Ok(())
    }
}

impl Drop for GnmiTargetSink {
    fn drop(&mut self) {
        if !self.joined {
            self.target.close_sink(self.owner);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::RwLock;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{
        BaseScheduleConfig, DistributionConfig, HistogramScenarioConfig, ScenarioConfig,
        SummaryScenarioConfig,
    };
    use crate::encoder::gnmi::GnmiEncoderConfig;
    use crate::encoder::EncoderConfig;
    use crate::generator::GeneratorConfig;
    use crate::model::metric::Labels;
    use crate::schedule::stats::ScenarioStats;
    use crate::sink::SinkConfig;
    use crate::OnSinkError;

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::same_address("127.0.0.1:57400", "127.0.0.1:57400", true)]
    #[case::v4_wildcard_and_v4("0.0.0.0:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_and_v4("[::]:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_and_v4_wildcard("[::]:57400", "0.0.0.0:57400", true)]
    #[case::v6_wildcard_and_v6("[::]:57400", "[::1]:57400", true)]
    #[case::v4_wildcard_and_v6("0.0.0.0:57400", "[::1]:57400", false)]
    #[case::two_specific_addresses("127.0.0.1:57400", "127.0.0.2:57400", false)]
    #[case::different_ports("0.0.0.0:57400", "127.0.0.1:57401", false)]
    #[case::port_zero("0.0.0.0:0", "127.0.0.1:0", false)]
    fn listen_overlap_is_per_family(#[case] a: &str, #[case] b: &str, #[case] overlap: bool) {
        let (a, b): (SocketAddr, SocketAddr) = (a.parse().unwrap(), b.parse().unwrap());
        assert_eq!(listen_overlap(a, b), overlap, "{a} and {b}");
        assert_eq!(listen_overlap(b, a), overlap, "{b} and {a}");
    }

    /// `covers` is one way: a wildcard covers the addresses of the families
    /// it accepts, never the other way round.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::v4_wildcard_covers_v4("0.0.0.0:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_covers_v4("[::]:57400", "127.0.0.1:57400", true)]
    #[case::v6_wildcard_covers_v4_wildcard("[::]:57400", "0.0.0.0:57400", true)]
    #[case::v4_wildcard_not_v6_wildcard("0.0.0.0:57400", "[::]:57400", false)]
    #[case::v4_wildcard_not_v6("0.0.0.0:57400", "[::1]:57400", false)]
    #[case::specific_covers_nothing("127.0.0.1:57400", "127.0.0.1:57400", false)]
    #[case::other_port("0.0.0.0:57400", "127.0.0.1:57401", false)]
    #[case::port_zero("0.0.0.0:0", "127.0.0.1:0", false)]
    fn covers_is_one_way(#[case] wild: &str, #[case] other: &str, #[case] covered: bool) {
        let (wild, other): (SocketAddr, SocketAddr) = (wild.parse().unwrap(), other.parse().unwrap());
        assert_eq!(covers(wild, other), covered, "{wild} covers {other}");
    }

    async fn sink() -> GnmiTargetSink {
        GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port")
    }

    fn event(name: &str) -> MetricEvent {
        MetricEvent::new(name.to_string(), 1.0, Labels::default()).expect("valid event")
    }

    /// `event` encoded by a gnmi encoder rendering `/a/{name}`.
    fn notification(event: &MetricEvent) -> Vec<u8> {
        use crate::encoder::gnmi::GnmiEncoder;
        use crate::encoder::Encoder;

        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            ..Default::default()
        })
        .expect("valid encoder");
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    async fn publish(sink: &mut GnmiTargetSink, name: &str) {
        let event = event(name);
        sink.write_metric_event(&event, &notification(&event))
            .await
            .expect("the write succeeds");
    }

    /// Counts calls on the way through to the wrapped sink, delegating
    /// `wants_metric_events` so the runner routes exactly as for the real sink.
    struct Counting {
        inner: GnmiTargetSink,
        writes: Arc<AtomicUsize>,
        events: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Sink for Counting {
        async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.inner.write(data).await
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            self.inner.flush().await
        }
        fn last_write_delivered(&self) -> bool {
            self.inner.last_write_delivered()
        }
        fn wants_metric_events(&self) -> bool {
            self.inner.wants_metric_events()
        }
        async fn write_metric_event(
            &mut self,
            event: &MetricEvent,
            encoded: &[u8],
        ) -> Result<(), SondaError> {
            self.events.fetch_add(1, Ordering::SeqCst);
            self.inner.write_metric_event(event, encoded).await
        }
    }

    fn base(name: &str, rate: f64, duration: &str) -> BaseScheduleConfig {
        BaseScheduleConfig {
            gap_windows: None,
            name: name.to_string(),
            rate,
            duration: Some(duration.to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::GnmiTarget(GnmiTargetConfig::new("127.0.0.1:0")),
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        }
    }

    fn encoder(path: &str, paths: &[(&str, &str)]) -> EncoderConfig {
        EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some(path.to_string()),
            paths: paths
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn write_without_event_is_an_error() {
        let mut sink = sink().await;
        let err = sink
            .write(b"bytes")
            .await
            .expect_err("raw bytes cannot be published");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(err.to_string().contains("gnmi encoder"), "got {err}");
        assert_eq!(sink.target().series_count(), 0);
    }

    /// A write is delivered once its event is in the target's table,
    /// whether or not anyone is subscribed.
    #[tokio::test]
    async fn last_write_delivered_after_an_upsert() {
        let mut sink = sink().await;
        assert!(!sink.last_write_delivered(), "nothing written yet");

        publish(&mut sink, "up").await;
        assert_eq!(sink.target().subscriber_count(), 0);
        assert!(
            sink.last_write_delivered(),
            "upserted with nobody subscribed"
        );

        let receiver = sink.target().subscribe();
        publish(&mut sink, "up").await;
        assert!(sink.last_write_delivered());
        drop(receiver);
        assert_eq!(sink.target().series_count(), 1);
    }

    /// A failed write is not delivered, even right after one that was.
    #[tokio::test]
    async fn a_failed_write_clears_last_write_delivered() {
        let mut sink = sink().await;
        publish(&mut sink, "up").await;
        assert!(sink.last_write_delivered());
        sink.write_metric_event(&event("down"), b"not protobuf")
            .await
            .expect_err("a series needs a path");
        assert!(!sink.last_write_delivered(), "the last write failed");
    }

    #[tokio::test]
    async fn first_event_that_is_not_a_notification_is_an_error() {
        let mut sink = sink().await;
        let err = sink
            .write_metric_event(&event("up"), b"not protobuf")
            .await
            .expect_err("a series needs a path");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        assert!(!sink.last_write_delivered());
        assert_eq!(sink.target().series_count(), 0);
    }

    /// Dropping a sink removes the series it published and leaves another
    /// sink's series, on the same target, in place.
    #[tokio::test]
    async fn drop_removes_the_sinks_series() {
        let mut first = sink().await;
        let addr = first.target().local_addr().to_string();
        let mut second = GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
            .await
            .expect("join the live target");
        let target = Arc::clone(first.target());
        assert!(Arc::ptr_eq(&target, second.target()));

        publish(&mut first, "in_octets").await;
        publish(&mut first, "out_octets").await;
        publish(&mut second, "in_errors").await;
        assert_eq!(target.series_count(), 3);

        drop(first);
        assert_eq!(target.series_count(), 1, "the first sink's two series left");
        let remaining: Vec<String> = target
            .snapshot()
            .into_iter()
            .map(|s| s.path.to_string())
            .collect();
        assert_eq!(remaining, ["openconfig:/a/in-errors"]);

        drop(second);
        assert_eq!(target.series_count(), 0);
    }

    /// Two sinks on one target, the first of which has published `/a/up`,
    /// and the error the second gets for its first write of the same path.
    async fn second_sink_on_an_owned_path() -> (GnmiTargetSink, GnmiTargetSink, SondaError) {
        let mut first = sink().await;
        let addr = first.target().local_addr().to_string();
        let mut second = GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
            .await
            .expect("acquire the live target");
        publish(&mut first, "up").await;
        let event = event("up");
        let err = second
            .write_metric_event(&event, &notification(&event))
            .await
            .expect_err("the path belongs to the first sink");
        (first, second, err)
    }

    /// A second sink's first write of a path another sink owns is a config
    /// error naming the path and the address.
    #[tokio::test]
    async fn a_second_sink_on_an_owned_path_is_refused() {
        let (first, second, err) = second_sink_on_an_owned_path().await;
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains(":/a/up`"), "names the path: {msg}");
        let addr = first.target().local_addr().to_string();
        assert!(msg.contains(&addr), "names the address: {msg}");
        assert!(!second.last_write_delivered());
    }

    /// The owner of a path keeps publishing it after another sink was
    /// refused.
    #[tokio::test]
    async fn the_owner_is_untouched_by_a_refused_sink() {
        let (mut first, _second, _) = second_sink_on_an_owned_path().await;
        publish(&mut first, "up").await;
        assert!(first.last_write_delivered());
        assert_eq!(first.target().series_count(), 1);
    }

    /// Dropping the refused sink leaves the owner's series in the table.
    #[tokio::test]
    async fn dropping_a_refused_sink_leaves_the_series() {
        let (first, second, _) = second_sink_on_an_owned_path().await;
        drop(second);
        assert_eq!(first.target().series_count(), 1);
        let target = Arc::clone(first.target());
        drop(first);
        assert_eq!(target.series_count(), 0, "the series leaves with its owner");
    }

    /// Two sinks of one launch share a path. When one drops, the series
    /// stays and another launch is still refused it; when the second drops,
    /// the series leaves and the port closes within the teardown bound.
    #[tokio::test]
    async fn a_launchs_series_leaves_with_its_last_sink() {
        let launch = LaunchId::next();
        let mut first = GnmiTargetSink::new_in(&GnmiTargetConfig::new("127.0.0.1:0"), launch)
            .await
            .expect("bind an ephemeral port");
        let addr = first.target().local_addr();
        let mut second = GnmiTargetSink::new_in(&GnmiTargetConfig::new(addr.to_string()), launch)
            .await
            .expect("acquire the live target");
        let target = Arc::clone(first.target());

        publish(&mut first, "up").await;
        publish(&mut second, "up").await;
        assert_eq!(target.series_count(), 1, "one launch, one shared series");

        drop(first);
        let paths: Vec<String> = target
            .snapshot()
            .iter()
            .map(|series| format!("{:?}", series.path))
            .collect();
        assert_eq!(paths.len(), 1, "the series outlives one of its sinks");
        assert!(paths[0].contains("\"up\""), "got {paths:?}");
        let mut intruder = GnmiTargetSink::new(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .expect("acquire the live target");
        let event = event("up");
        let err = intruder
            .write_metric_event(&event, &notification(&event))
            .await
            .expect_err("the path still belongs to the launch");
        assert!(err.to_string().contains("already published"), "got {err}");
        drop(intruder);
        publish(&mut second, "up").await;
        assert!(second.last_write_delivered());

        drop(second);
        assert_eq!(target.series_count(), 0, "the series left with the launch");
        let stopped = target.wait();
        drop(target);
        tokio::time::timeout(std::time::Duration::from_secs(2), stopped)
            .await
            .expect("the listener stops within the teardown bound");
        tokio::net::TcpListener::bind(addr)
            .await
            .expect("the port is free again");
    }

    /// One sink whose series render one leaf, once a label is dropped,
    /// upserts that leaf.
    #[tokio::test]
    async fn one_sink_publishing_a_leaf_twice_upserts_it() {
        use crate::encoder::gnmi::GnmiEncoder;
        use crate::encoder::Encoder;

        let mut sink = sink().await;
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            path: Some("/a/{name}".to_string()),
            drop_labels: vec!["job".to_string()],
            ..Default::default()
        })
        .expect("valid encoder");
        for (job, value) in [("a", 1.0), ("b", 2.0)] {
            let event = MetricEvent::new(
                "up".to_string(),
                value,
                Labels::from_pairs(&[("job", job)]).expect("valid labels"),
            )
            .expect("valid event");
            let mut buf = Vec::new();
            encoder.encode_metric(&event, &mut buf).expect("encodes");
            sink.write_metric_event(&event, &buf)
                .await
                .expect("the sink's own leaf");
        }
        assert_eq!(sink.owned.len(), 2, "two series keys");
        assert_eq!(sink.target().series_count(), 1, "one leaf");
        let target = Arc::clone(sink.target());
        drop(sink);
        assert_eq!(target.series_count(), 0);
    }

    #[tokio::test]
    async fn scenario_delivers_every_event_through_the_hook() {
        let writes = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let inner = sink().await;
        let target = Arc::clone(inner.target());
        let mut sink: Box<dyn Sink> = Box::new(Counting {
            inner,
            writes: Arc::clone(&writes),
            events: Arc::clone(&events),
        });
        let config = ScenarioConfig {
            base: base("in_octets", 1_000.0, "250ms"),
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: encoder("/a/{name}", &[]),
            metric_type: None,
            help: None,
        };
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect("the scenario runs to completion");

        let ticks = stats.read().unwrap().total_events as usize;
        assert!(ticks >= 100, "only {ticks} ticks ran");
        assert_eq!(
            events.load(Ordering::SeqCst),
            ticks,
            "every event used the hook"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "nothing fell back to write"
        );
        assert_eq!(target.series_count(), 1);
    }

    /// A scenario whose first write lands on a path another sink owns ends
    /// with that error recorded in its stats, and the owner keeps the path.
    #[tokio::test]
    async fn a_scenario_refused_a_path_records_why_it_stopped() {
        let mut owner = sink().await;
        publish(&mut owner, "in_octets").await;
        let addr = owner.target().local_addr().to_string();
        let mut sink: Box<dyn Sink> = Box::new(
            GnmiTargetSink::new(&GnmiTargetConfig::new(addr))
                .await
                .expect("acquire the live target"),
        );
        let config = ScenarioConfig {
            base: base("in_octets", 100.0, "1s"),
            generator: GeneratorConfig::Constant { value: 1.0 },
            encoder: encoder("/a/{name}", &[]),
            metric_type: None,
            help: None,
        };
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let err = crate::schedule::runner::run_with_sink(
            &config,
            &mut sink,
            &CancellationToken::new(),
            Some(Arc::clone(&stats)),
        )
        .await
        .expect_err("the path belongs to the owner");

        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let st = stats.read().expect("stats lock");
        assert_eq!(st.errors, 1, "the refusal is counted");
        assert_eq!(st.total_events, 0);
        let reason = st.last_sink_error.clone().unwrap_or_default();
        assert!(reason.contains(":/a/in-octets`"), "got {reason:?}");
        drop(st);
        drop(sink);
        assert_eq!(owner.target().series_count(), 1, "the owner keeps the path");
    }

    /// The `le` / `quantile` key of every bucket or quantile path, and the
    /// names of the series that have none.
    fn keyed_paths(target: &GnmiTarget, key: &str) -> (Vec<String>, Vec<String>) {
        let mut keyed = Vec::new();
        let mut plain = Vec::new();
        for series in target.snapshot() {
            let last = series.path.elem.last().expect("a path has elements");
            match series.path.elem.iter().find_map(|e| e.key.get(key)) {
                Some(value) => keyed.push(value.clone()),
                None => plain.push(last.name.clone()),
            }
        }
        keyed.sort();
        plain.sort();
        (keyed, plain)
    }

    /// The entry kinds whose runner injects a label per series.
    enum Distribution {
        Histogram,
        Summary,
    }

    /// One tick of a histogram (4 buckets) or summary (3 quantiles) lands
    /// one series per bucket or quantile, keyed by `le` or `quantile`, plus
    /// `_sum` and `_count` on their own paths.
    #[rstest::rstest]
    #[case::histogram(Distribution::Histogram, "le", &["+Inf", "0.1", "0.5", "1", "5"])]
    #[case::summary(Distribution::Summary, "quantile", &["0.5", "0.9", "0.99"])]
    #[tokio::test]
    async fn histogram_scenario_lands_one_series_per_bucket(
        #[case] kind: Distribution,
        #[case] key: &str,
        #[case] expected: &[&str],
    ) {
        let inner = sink().await;
        let target = Arc::clone(inner.target());
        let mut sink: Box<dyn Sink> = Box::new(inner);
        let encoder = encoder(
            &format!("/latency[{key}={{{key}}}]/{{name}}"),
            &[
                ("latency_sum", "/latency/{name}"),
                ("latency_count", "/latency/{name}"),
            ],
        );
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let cancel = CancellationToken::new();
        match kind {
            Distribution::Histogram => {
                let config = HistogramScenarioConfig {
                    base: base("latency", 10.0, "150ms"),
                    buckets: Some(vec![0.1, 0.5, 1.0, 5.0]),
                    distribution: DistributionConfig::Exponential { rate: 10.0 },
                    observations_per_tick: Some(100),
                    mean_shift_per_sec: None,
                    seed: Some(42),
                    encoder,
                    metric_type: None,
                    help: None,
                };
                crate::config::validate::validate_histogram_config(&config)
                    .expect("the entry validates");
                crate::schedule::histogram_runner::run_with_sink(
                    &config,
                    &mut sink,
                    &cancel,
                    Some(Arc::clone(&stats)),
                )
                .await
            }
            Distribution::Summary => {
                let config = SummaryScenarioConfig {
                    base: base("latency", 10.0, "150ms"),
                    quantiles: Some(vec![0.5, 0.9, 0.99]),
                    distribution: DistributionConfig::Exponential { rate: 10.0 },
                    observations_per_tick: Some(100),
                    mean_shift_per_sec: None,
                    seed: Some(42),
                    encoder,
                    metric_type: None,
                    help: None,
                };
                crate::config::validate::validate_summary_config(&config)
                    .expect("the entry validates");
                crate::schedule::summary_runner::run_with_sink(
                    &config,
                    &mut sink,
                    &cancel,
                    Some(Arc::clone(&stats)),
                )
                .await
            }
        }
        .expect("the scenario runs to completion");
        assert_eq!(stats.read().unwrap().rejected_events, 0);

        assert_eq!(
            target.series_count(),
            expected.len() + 2,
            "one series per {key} value, plus sum and count"
        );
        let (keyed, plain) = keyed_paths(&target, key);
        assert_eq!(keyed, expected);
        assert_eq!(plain, ["latency-count", "latency-sum"]);
    }
}
