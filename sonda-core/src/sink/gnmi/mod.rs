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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::validate::{parse_duration, prepend_context};
use crate::model::metric::MetricEvent;
use crate::sink::Sink;
use crate::{ConfigError, SondaError};

pub use registry::{registry, GnmiTargetRegistry};
pub use target::{GnmiTarget, SeriesId, SeriesUpdate};

/// Streams allowed per target when `max_subscribers` is not set.
pub const DEFAULT_MAX_SUBSCRIBERS: usize = 16;

/// Broadcast capacity per target when `subscriber_buffer` is not set.
pub const DEFAULT_SUBSCRIBER_BUFFER: usize = 1024;

/// Largest accepted `subscriber_buffer`. The broadcast ring is allocated up
/// front at this many slots.
pub const MAX_SUBSCRIBER_BUFFER: usize = 1 << 20;

/// Smallest `sample_interval` honoured when `sample_floor` is not set.
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
    #[cfg_attr(feature = "config", serde(deserialize_with = "scalar_text::required"))]
    pub listen: String,
    /// Streams beyond this are refused with `RESOURCE_EXHAUSTED`. Default 16.
    #[cfg_attr(feature = "config", serde(default))]
    pub max_subscribers: Option<usize>,
    /// Broadcast capacity per target, 1 to 1048576. Default 1024.
    #[cfg_attr(feature = "config", serde(default))]
    pub subscriber_buffer: Option<usize>,
    /// Smallest `sample_interval` honoured, e.g. `"1s"`. Default `"1s"`.
    #[cfg_attr(
        feature = "config",
        serde(default, deserialize_with = "scalar_text::optional")
    )]
    pub sample_floor: Option<String>,
}

/// Deserializers that take a string, number or boolean as its display text
/// (`57400` becomes `"57400"`, `1.0` becomes `"1"`), so every value the
/// generated schema accepts for a text field parses, and the field's own
/// validation reports what is wrong with it.
#[cfg(feature = "config")]
mod scalar_text {
    use std::fmt;

    use serde::de::{Deserializer, Error, Visitor};
    use serde::Deserialize;

    struct ScalarText(String);

    impl<'de> Deserialize<'de> for ScalarText {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct TextVisitor;
            impl Visitor<'_> for TextVisitor {
                type Value = ScalarText;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a string, number or boolean")
                }
                fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                    Ok(ScalarText(v.to_string()))
                }
                fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                    Ok(ScalarText(v))
                }
                fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                    Ok(ScalarText(v.to_string()))
                }
                fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
                    Ok(ScalarText(v.to_string()))
                }
                fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
                    Ok(ScalarText(v.to_string()))
                }
                fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
                    Ok(ScalarText(v.to_string()))
                }
            }
            deserializer.deserialize_any(TextVisitor)
        }
    }

    /// A required text field.
    pub(super) fn required<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
        ScalarText::deserialize(deserializer).map(|text| text.0)
    }

    /// An optional text field; `null` is `None`.
    pub(super) fn optional<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<String>, D::Error> {
        Option::<ScalarText>::deserialize(deserializer).map(|text| text.map(|text| text.0))
    }
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
    /// duration greater than zero, `max_subscribers` is 0, or
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
        // parse_duration refuses zero, so the floor is always positive.
        let sample_floor = match self.sample_floor.as_deref() {
            None => DEFAULT_SAMPLE_FLOOR,
            Some(s) => parse_duration(s)
                .map_err(|e| prepend_context("invalid gnmi_target sample_floor", s, e))?,
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

impl TargetSettings {
    /// Whether a target running with `self` can also serve `other`: every
    /// setting but the listen address is equal.
    pub(crate) fn serves(&self, other: &TargetSettings) -> bool {
        self.max_subscribers == other.max_subscribers
            && self.subscriber_buffer == other.subscriber_buffer
            && self.sample_floor == other.sample_floor
    }
}

/// Publishes metric events into the shared [`GnmiTarget`] for its listen address.
///
/// Only [`Sink::write_metric_event`] carries data: the sink needs the event's
/// series identity, so it opts in through [`Sink::wants_metric_events`] and
/// [`Sink::write`] is an error. Dropping the last sink on an address stops
/// its listener.
pub struct GnmiTargetSink {
    target: Arc<GnmiTarget>,
    last_delivered: bool,
}

impl GnmiTargetSink {
    /// Join the target for `cfg.listen`, binding it if no live target has
    /// that address.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or a live target on the
    /// same address with different settings; [`SondaError::Sink`] when the
    /// address cannot be bound.
    pub async fn new(cfg: &GnmiTargetConfig) -> Result<Self, SondaError> {
        Ok(Self {
            target: registry().acquire(cfg).await?,
            last_delivered: false,
        })
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

    /// `true` when the target had at least one subscriber at the last write.
    fn last_write_delivered(&self) -> bool {
        self.last_delivered
    }

    fn wants_metric_events(&self) -> bool {
        true
    }

    /// Publish `encoded` as the latest value of the event's series. Never awaits.
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        let key = (event.name.clone(), Arc::clone(&event.labels));
        self.target.publish(&key, encoded);
        self.last_delivered = self.target.subscriber_count() > 0;
        Ok(())
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

    async fn sink() -> GnmiTargetSink {
        GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port")
    }

    fn event(name: &str) -> MetricEvent {
        MetricEvent::new(name.to_string(), 1.0, Labels::default()).expect("valid event")
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

    #[tokio::test]
    async fn last_write_delivered_tracks_subscribers() {
        let mut sink = sink().await;
        sink.write_metric_event(&event("up"), b"a").await.unwrap();
        assert!(!sink.last_write_delivered(), "nobody is subscribed");

        let receiver = sink.target().subscribe();
        sink.write_metric_event(&event("up"), b"b").await.unwrap();
        assert!(sink.last_write_delivered(), "one subscriber");

        drop(receiver);
        sink.write_metric_event(&event("up"), b"c").await.unwrap();
        assert!(!sink.last_write_delivered(), "the subscriber left");
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
