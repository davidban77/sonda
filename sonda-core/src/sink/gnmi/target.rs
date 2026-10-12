//! The shared state behind one gNMI listen address.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use bytes::Bytes;
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;

use super::service::GnmiService;
use super::TargetSettings;
use crate::encoder::gnmi::proto;
use crate::model::metric::MetricKey;
use crate::SondaError;

/// A target-local series number, allocated on a series' first publish.
pub type SeriesId = u32;

/// A new latest value for one series, as broadcast to subscribers.
#[derive(Clone, Debug)]
pub struct SeriesUpdate {
    /// The series the value belongs to.
    pub series_id: SeriesId,
    /// One encoded `gnmi.Notification`.
    pub bytes: Bytes,
}

/// One series as held by the target, returned by [`GnmiTarget::snapshot`].
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct SeriesSnapshot {
    pub(crate) series_id: SeriesId,
    /// The series' full path: the notification prefix's origin and target
    /// with the update's elements.
    pub(crate) path: proto::Path,
    /// The latest encoded `gnmi.Notification`.
    pub(crate) latest: Bytes,
}

/// One series in the table.
struct Slot {
    id: SeriesId,
    path: proto::Path,
    latest: Mutex<Bytes>,
}

#[derive(Default)]
struct SeriesTable {
    slots: HashMap<MetricKey, Slot>,
    next_id: SeriesId,
}

/// The latest value of every series published on one listen address, the
/// broadcast that carries new values to subscribers, and the gRPC listener.
///
/// Obtained from the [`registry`](super::registry::registry). The listener
/// runs on its own task until the last `Arc<GnmiTarget>` is dropped, which
/// cancels the target's root token: the server stops accepting and every
/// task spawned on a child of that token ends.
pub struct GnmiTarget {
    local_addr: SocketAddr,
    settings: TargetSettings,
    table: RwLock<SeriesTable>,
    tx: broadcast::Sender<SeriesUpdate>,
    token: CancellationToken,
    stopped: watch::Receiver<bool>,
}

impl GnmiTarget {
    /// Bind `settings.listen` and start serving on a spawned task.
    pub(crate) async fn bind(settings: TargetSettings) -> Result<Arc<Self>, SondaError> {
        let listen = settings.listen;
        let listener = TcpListener::bind(listen).await.map_err(|e| {
            SondaError::Sink(std::io::Error::new(
                e.kind(),
                format!("gnmi_target cannot listen on {listen}: {e}"),
            ))
        })?;
        let local_addr = listener.local_addr().map_err(SondaError::Sink)?;
        let (tx, _) = broadcast::channel(settings.subscriber_buffer);
        let token = CancellationToken::new();
        let (stopped_tx, stopped) = watch::channel(false);

        Ok(Arc::new_cyclic(|target| {
            let service = GnmiService::new(target.clone());
            let shutdown = token.clone().cancelled_owned();
            tokio::spawn(async move {
                let served = tonic::transport::Server::builder()
                    .serve_with_incoming_shutdown(
                        service,
                        TcpListenerStream::new(listener),
                        shutdown,
                    )
                    .await;
                if let Err(e) = served {
                    tracing::warn!(error = %e, addr = %local_addr, "gnmi_target server stopped with an error");
                }
                let _ = stopped_tx.send(true);
            });
            GnmiTarget {
                local_addr,
                settings,
                table: RwLock::new(SeriesTable::default()),
                tx,
                token,
                stopped,
            }
        }))
    }

    /// The address the listener bound, with an ephemeral port resolved.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Receivers currently subscribed to the broadcast.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Distinct series published so far.
    pub fn series_count(&self) -> usize {
        self.read().slots.len()
    }

    /// Resolves once the listener task has exited. The returned future does
    /// not hold the target, so it can be awaited after the last handle is
    /// dropped.
    pub fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut stopped = self.stopped.clone();
        async move {
            let _ = stopped.wait_for(|stopped| *stopped).await;
        }
    }

    /// The settings the target was bound with.
    pub(crate) fn settings(&self) -> &TargetSettings {
        &self.settings
    }

    /// Upsert the latest encoded notification for a series; allocates the
    /// `SeriesId` on first sight.
    ///
    /// Synchronous. Takes the table write lock only on a series' first
    /// publish; `broadcast::Sender::send` never blocks. Copies `encoded`
    /// once. The path stored for a new series is decoded from `encoded`; a
    /// buffer that is not a `gnmi.Notification` with an update is stored
    /// with an empty path.
    pub(crate) fn publish(&self, key: &MetricKey, encoded: &[u8]) -> SeriesId {
        let latest = Bytes::copy_from_slice(encoded);
        // A clone shares the buffer but allocates its refcount on first use,
        // so it is made only when someone will receive it.
        let update = (self.tx.receiver_count() > 0).then(|| latest.clone());
        let series_id = self.store(key, latest);
        if let Some(bytes) = update {
            // An error means every receiver has gone; there is nobody to tell.
            let _ = self.tx.send(SeriesUpdate { series_id, bytes });
        }
        series_id
    }

    /// A receiver for every update published from now on.
    #[allow(dead_code)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SeriesUpdate> {
        self.tx.subscribe()
    }

    /// Every series with its path and latest value, in no particular order.
    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> Vec<SeriesSnapshot> {
        self.read()
            .slots
            .values()
            .map(|slot| SeriesSnapshot {
                series_id: slot.id,
                path: slot.path.clone(),
                latest: lock(&slot.latest).clone(),
            })
            .collect()
    }

    fn store(&self, key: &MetricKey, latest: Bytes) -> SeriesId {
        if let Some(slot) = self.read().slots.get(key) {
            *lock(&slot.latest) = latest;
            return slot.id;
        }
        let mut table = self.table.write().unwrap_or_else(PoisonError::into_inner);
        // Another writer may have inserted the series since the read above.
        if let Some(slot) = table.slots.get(key) {
            *lock(&slot.latest) = latest;
            return slot.id;
        }
        let id = table.next_id;
        table.next_id = table.next_id.wrapping_add(1);
        let path = series_path(&latest);
        table.slots.insert(
            key.clone(),
            Slot {
                id,
                path,
                latest: Mutex::new(latest),
            },
        );
        id
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, SeriesTable> {
        self.table.read().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for GnmiTarget {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

fn lock(latest: &Mutex<Bytes>) -> std::sync::MutexGuard<'_, Bytes> {
    latest.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The full path of the first update in an encoded notification: the
/// prefix's origin and target with the update's elements.
fn series_path(encoded: &[u8]) -> proto::Path {
    let Ok(mut notification) = proto::Notification::decode(encoded) else {
        return proto::Path::default();
    };
    let prefix = notification.prefix.take().unwrap_or_default();
    let elem = notification
        .update
        .into_iter()
        .next()
        .and_then(|update| update.path)
        .map(|path| path.elem)
        .unwrap_or_default();
    proto::Path {
        origin: prefix.origin,
        elem,
        target: prefix.target,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::model::metric::{Labels, ValidatedMetricName};
    use crate::sink::gnmi::GnmiTargetConfig;

    async fn bind(subscriber_buffer: usize) -> Arc<GnmiTarget> {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        cfg.subscriber_buffer = Some(subscriber_buffer);
        GnmiTarget::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    fn key(name: &str, pairs: &[(&str, &str)]) -> MetricKey {
        (
            ValidatedMetricName::new(name).expect("valid name"),
            Arc::new(Labels::from_pairs(pairs).expect("valid labels")),
        )
    }

    #[tokio::test]
    async fn publish_allocates_id_once_per_series() {
        let target = bind(16).await;
        let a = key("in_octets", &[("ifName", "Gi0")]);
        let b = key("in_octets", &[("ifName", "Gi1")]);

        let first = target.publish(&a, b"one");
        let again = target.publish(&a, b"two");
        let other = target.publish(&b, b"three");

        assert_eq!(first, again, "the same key keeps its id");
        assert_ne!(first, other, "different labels are a different series");
        assert_eq!(target.series_count(), 2);
        let latest: Vec<Bytes> = {
            let mut snapshot = target.snapshot();
            snapshot.sort_by_key(|s| s.series_id);
            snapshot.into_iter().map(|s| s.latest).collect()
        };
        assert_eq!(
            latest,
            [Bytes::from_static(b"two"), Bytes::from_static(b"three")]
        );
    }

    #[tokio::test]
    async fn publish_never_blocks_without_receivers() {
        let target = bind(4).await;
        let series = key("in_octets", &[]);
        let started = Instant::now();
        let mut published = 0usize;
        for _ in 0..10_000 {
            target.publish(&series, b"value");
            published += 1;
        }
        let elapsed = started.elapsed();
        assert_eq!(published, 10_000);
        assert_eq!(target.subscriber_count(), 0);
        assert!(
            elapsed < Duration::from_millis(100),
            "10 000 publishes took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn lagged_receiver_does_not_block_publisher() {
        let target = bind(4).await;
        let mut receiver = target.subscribe();
        let series = key("in_octets", &[]);
        for _ in 0..1_000 {
            target.publish(&series, b"value");
        }
        match receiver.recv().await {
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                assert!(skipped > 0, "a lag reports what it skipped");
            }
            other => panic!("a receiver that never read must have lagged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_carries_the_decoded_series_path() {
        use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
        use crate::encoder::Encoder;
        use crate::model::metric::MetricEvent;

        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            origin: "openconfig".to_string(),
            path: Some("/interfaces/interface[name={ifName}]/{name}".to_string()),
            ..Default::default()
        })
        .expect("valid encoder");
        let series = key("in_octets", &[("device", "rtr1"), ("ifName", "Gi0")]);
        let event = MetricEvent::from_parts(
            series.0.clone(),
            1.0,
            Arc::clone(&series.1),
            std::time::SystemTime::now(),
        );
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");

        let target = bind(16).await;
        target.publish(&series, &buf);
        let snapshot = target.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].path.to_string(),
            "openconfig:/interfaces/interface[name=Gi0]/in-octets"
        );
        assert_eq!(snapshot[0].path.target, "rtr1");
    }
}
