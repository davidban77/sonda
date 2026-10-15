//! The shared state behind one gNMI listen address.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tonic::transport::server::{Connected, TcpConnectInfo};

use super::service::GnmiService;
use super::TargetSettings;
use crate::encoder::gnmi::proto;
use crate::schedule::LaunchId;
use crate::{ConfigError, SondaError};

/// How long a connection still open when its target stops may stay open
/// before the target closes it.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// How long the listener waits before accepting again after an accept
/// error that is not about one connection, such as running out of file
/// descriptors.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Whether an accept error concerns only the connection being accepted, so
/// the next accept can follow at once.
fn is_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

/// Where a target's listener is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetState {
    /// Accepting connections.
    Listening,
    /// The listening socket is closed; open connections have up to
    /// [`DRAIN_GRACE`] to finish before they are closed.
    Draining,
    /// The listener task has exited and every connection is closed.
    Stopped,
}

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

/// The gNMI identity of a series: its full path, with the origin and target
/// name. Two events whose notifications carry one path are one series.
#[derive(Clone, Debug)]
pub(crate) struct PathKey(Arc<proto::Path>);

impl PathKey {
    /// A key for `path`.
    pub(crate) fn new(path: proto::Path) -> Self {
        Self(Arc::new(path))
    }

    /// The path of the first update in an encoded `gnmi.Notification`: the
    /// prefix's origin and target with the update's elements.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when `encoded` is not a notification with an
    /// update.
    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, SondaError> {
        let not_a_notification = |detail: String| {
            SondaError::Sink(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "gnmi_target received bytes that are not a gnmi notification with an \
                     update: {detail}"
                ),
            ))
        };
        let mut notification =
            proto::Notification::decode(encoded).map_err(|e| not_a_notification(e.to_string()))?;
        let prefix = notification.prefix.take().unwrap_or_default();
        let elem = notification
            .update
            .into_iter()
            .next()
            .and_then(|update| update.path)
            .ok_or_else(|| not_a_notification("no update path".to_string()))?
            .elem;
        Ok(Self::new(proto::Path {
            origin: prefix.origin,
            elem,
            target: prefix.target,
        }))
    }

    /// The path.
    pub(crate) fn path(&self) -> &proto::Path {
        &self.0
    }

    /// Whether `self` and `other` are one key, not just equal ones.
    #[cfg(test)]
    pub(crate) fn is_same_key(&self, other: &PathKey) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for PathKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Eq for PathKey {}

impl Hash for PathKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.origin.hash(state);
        self.0.target.hash(state);
        self.0.elem.len().hash(state);
        for elem in &self.0.elem {
            elem.name.hash(state);
            elem.key.hash(state);
        }
    }
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

/// The launch a series belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner(u64);

impl From<LaunchId> for Owner {
    fn from(launch: LaunchId) -> Self {
        Self(launch.get())
    }
}

/// One series in the table.
struct Slot {
    path: PathKey,
    latest: Mutex<Bytes>,
    /// The only launch whose sinks may publish the series.
    owner: Owner,
    /// Sinks of `owner` that have published the series; it leaves the table
    /// when the last of them is released.
    publishers: usize,
}

#[derive(Default)]
struct SeriesTable {
    ids: HashMap<PathKey, SeriesId>,
    slots: HashMap<SeriesId, Slot>,
    next_id: SeriesId,
}

/// The error the listener task exited with, if any.
type ServeError = Arc<Mutex<Option<String>>>;

/// The latest value of every series published on one listen address, keyed
/// by the series' path, the broadcast that carries new values to
/// subscribers, and the gRPC listener.
///
/// Obtained from the [`registry`](super::registry::registry). The listener
/// runs on its own task until the last `Arc<GnmiTarget>` is dropped, which
/// cancels the target's root token: the listening socket closes at once,
/// and connections still open after [`DRAIN_GRACE`] are closed.
///
/// A series is owned by one launch and held while any sink of that launch
/// has published it; it leaves when the last of those sinks drops, not when
/// it stops being updated. A path with no live publisher is free: another
/// launch may take it, and a scenario of the first launch refused it in that
/// window shows why in its stats.
pub struct GnmiTarget {
    local_addr: SocketAddr,
    settings: TargetSettings,
    table: RwLock<SeriesTable>,
    tx: broadcast::Sender<SeriesUpdate>,
    token: CancellationToken,
    state: watch::Receiver<TargetState>,
    error: ServeError,
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
        if listen.port() == 0 {
            tracing::warn!(addr = %local_addr, "gnmi_target listening on an ephemeral port");
        } else {
            tracing::info!(addr = %local_addr, "gnmi_target listening");
        }
        let (tx, _) = broadcast::channel(settings.subscriber_buffer);
        let token = CancellationToken::new();
        let (state_tx, state) = watch::channel(TargetState::Listening);
        let error = ServeError::default();

        Ok(Arc::new_cyclic(|target| {
            let service = GnmiService::new(target.clone());
            tokio::spawn(serve(
                service,
                listener,
                token.clone(),
                StoppedOnDrop(state_tx),
                Arc::clone(&error),
            ));
            GnmiTarget {
                local_addr,
                settings,
                table: RwLock::new(SeriesTable::default()),
                tx,
                token,
                state,
                error,
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

    /// Distinct series paths in the table.
    pub fn series_count(&self) -> usize {
        self.read().slots.len()
    }

    /// Resolves once the listener task has exited and every connection it
    /// accepted is closed. The returned future does not hold the target, so
    /// it can be awaited after the last handle is dropped.
    pub fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut state = self.state.clone();
        async move {
            let _ = state.wait_for(|s| *s == TargetState::Stopped).await;
        }
    }

    /// A receiver for the listener's lifecycle state.
    pub(crate) fn state(&self) -> watch::Receiver<TargetState> {
        self.state.clone()
    }

    /// Whether the listener task is still accepting connections. It is not
    /// once it has returned, panicked, or been dropped with its runtime.
    pub(crate) fn is_serving(&self) -> bool {
        *self.state.borrow() == TargetState::Listening
    }

    /// `Ok` while the listener task runs; otherwise [`SondaError::Sink`]
    /// naming the address and the error the task exited with.
    ///
    /// A sink whose listener stopped gets this error on every write, which
    /// the scenario records and its `on_sink_error` policy handles (`fail`
    /// ends it); nothing rebinds the address for that sink.
    pub(crate) fn check_serving(&self) -> Result<(), SondaError> {
        if self.is_serving() {
            return Ok(());
        }
        let error = self
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| "it was stopped".to_string());
        Err(SondaError::Sink(io::Error::new(
            io::ErrorKind::NotConnected,
            format!(
                "gnmi_target {} is no longer serving: {error}",
                self.local_addr
            ),
        )))
    }

    /// Stop the listener while handles to the target remain.
    #[cfg(test)]
    pub(crate) fn stop_serving(&self) {
        self.token.cancel();
    }

    /// The settings the target was bound with.
    pub(crate) fn settings(&self) -> &TargetSettings {
        &self.settings
    }

    /// Upsert `encoded` as the latest notification of a series `owner`
    /// publishes. Returns the series' id and whether any receiver was
    /// subscribed.
    ///
    /// With `id` naming a series `owner` holds, that series is updated under
    /// the read lock. With no `id`, this is a sink's first publish of `path`:
    /// the series is found or created under the write lock, with a new id and
    /// `owner` as its owner, and counts one more publisher. Each publisher is
    /// [released](Self::release) once; the series leaves with the last.
    ///
    /// Synchronous; `broadcast::Sender::send` never blocks. Copies `encoded`
    /// once. The update is broadcast while the series is still locked, so
    /// concurrent publishes of one path reach subscribers in the order the
    /// table stores them.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] naming the path and the listen address when
    /// another launch owns the series for `path`.
    pub(crate) fn publish(
        &self,
        owner: Owner,
        id: Option<SeriesId>,
        path: &PathKey,
        encoded: &[u8],
    ) -> Result<(SeriesId, bool), SondaError> {
        self.store(owner, id, path, Bytes::copy_from_slice(encoded))
    }

    /// Replace the latest notification of the series already in the table
    /// for `path`, whoever owns it, taking no ownership. Returns its id, or
    /// `None` when no series has that path. Synchronous, and broadcast like
    /// [`publish`](Self::publish).
    pub(crate) fn overwrite(&self, path: &PathKey, encoded: &[u8]) -> Option<SeriesId> {
        let table = self.read();
        let id = *table.ids.get(path)?;
        let slot = table.slots.get(&id)?;
        self.replace(id, &mut lock(&slot.latest), Bytes::copy_from_slice(encoded));
        Some(id)
    }

    /// The key the table holds for series `id`, which a publisher caches so
    /// later publishes compare it by pointer.
    pub(crate) fn path_of(&self, id: SeriesId) -> Option<PathKey> {
        self.read().slots.get(&id).map(|slot| slot.path.clone())
    }

    /// Release one publisher of each of `ids` that `owner` owns; a series
    /// whose last publisher is released leaves the table. Ids that are unknown
    /// or owned by another launch are ignored. A sink releases each id it
    /// published once, when it drops.
    pub(crate) fn release(&self, owner: Owner, ids: impl IntoIterator<Item = SeriesId>) {
        let mut table = self.write();
        for id in ids {
            let Some(slot) = table.slots.get_mut(&id).filter(|slot| slot.owner == owner) else {
                continue;
            };
            slot.publishers = slot.publishers.saturating_sub(1);
            if slot.publishers == 0 {
                if let Some(slot) = table.slots.remove(&id) {
                    table.ids.remove(&slot.path);
                }
            }
        }
    }

    /// A receiver for every update published from now on. Subscribe before
    /// taking a [`snapshot`](Self::snapshot): every value the snapshot does
    /// not hold then arrives on the receiver.
    #[allow(dead_code)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SeriesUpdate> {
        self.tx.subscribe()
    }

    /// Every series with its path and latest value, in no particular order.
    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> Vec<SeriesSnapshot> {
        self.read()
            .slots
            .iter()
            .map(|(id, slot)| SeriesSnapshot {
                series_id: *id,
                path: slot.path.path().clone(),
                latest: lock(&slot.latest).clone(),
            })
            .collect()
    }

    fn store(
        &self,
        owner: Owner,
        id: Option<SeriesId>,
        path: &PathKey,
        latest: Bytes,
    ) -> Result<(SeriesId, bool), SondaError> {
        let requested = id;
        if let Some(id) = id {
            let table = self.read();
            // A cached id names this path's series unless the id was freed
            // and handed to another path since.
            if let Some(slot) = table
                .slots
                .get(&id)
                .filter(|slot| slot.owner == owner && slot.path == *path)
            {
                let delivered = self.replace(id, &mut lock(&slot.latest), latest);
                return Ok((id, delivered));
            }
        }
        let mut table = self.write();
        let id = match table.ids.get(path) {
            Some(id) => {
                if table.slots.get(id).is_some_and(|slot| slot.owner != owner) {
                    return Err(SondaError::Config(ConfigError::invalid(format!(
                        "the gnmi path `{}` on {} is already published by another running \
                         scenario",
                        path.path(),
                        self.local_addr
                    ))));
                }
                *id
            }
            None => {
                // After a wrap, skip ids still held by live series.
                let mut id = table.next_id;
                while table.slots.contains_key(&id) {
                    id = id.wrapping_add(1);
                }
                table.next_id = id.wrapping_add(1);
                table.ids.insert(path.clone(), id);
                id
            }
        };
        let slot = table.slots.entry(id).or_insert_with(|| Slot {
            path: path.clone(),
            latest: Mutex::new(Bytes::new()),
            owner,
            publishers: 0,
        });
        // A publish with no id is the publisher's first of this path.
        if requested.is_none() {
            slot.publishers += 1;
        }
        let current = slot
            .latest
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let delivered = self.replace(id, current, latest);
        Ok((id, delivered))
    }

    /// Store `latest` in `current` and, when anyone is subscribed, broadcast
    /// it. The caller holds the series' lock, so the store, the check for
    /// receivers and the broadcast are one step: a subscriber that takes a
    /// snapshot after subscribing sees the stored value or receives it.
    /// Returns whether any receiver was subscribed.
    fn replace(&self, id: SeriesId, current: &mut Bytes, latest: Bytes) -> bool {
        // A clone shares the buffer but allocates its refcount on first use,
        // so it is made only when someone will receive it.
        let update = (self.tx.receiver_count() > 0).then(|| latest.clone());
        *current = latest;
        let receivers = update.is_some();
        if let Some(bytes) = update {
            // An error means every receiver has gone; there is nobody to tell.
            let _ = self.tx.send(SeriesUpdate {
                series_id: id,
                bytes,
            });
        }
        receivers
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, SeriesTable> {
        self.table.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, SeriesTable> {
        self.table.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for GnmiTarget {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// Serve `service` on `listener` until `token` is cancelled, then close the
/// listening socket, give open connections [`DRAIN_GRACE`] to finish, and
/// close the rest. Reports each step on `state`.
async fn serve(
    service: GnmiService,
    listener: TcpListener,
    token: CancellationToken,
    state: StoppedOnDrop,
    error: ServeError,
) {
    let local_addr = listener.local_addr().ok();
    let kill = CancellationToken::new();
    // Every connection holds a sender; `recv` returns `None` once all of
    // them, and the one held by `Incoming`, are dropped.
    let (alive, mut connections) = mpsc::channel::<()>(1);
    let incoming = Incoming {
        listener: Some(listener),
        stop: Box::pin(token.cancelled_owned()),
        kill: kill.clone(),
        alive,
        backoff: None,
    };
    // Returns once `incoming` ends, which drops the listening socket.
    let served = tonic::transport::Server::builder()
        .serve_with_incoming(service, incoming)
        .await;
    if let Err(e) = served {
        tracing::warn!(error = %e, addr = ?local_addr, "gnmi_target server stopped with an error");
        *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
    }
    state.0.send_replace(TargetState::Draining);
    if tokio::time::timeout(DRAIN_GRACE, connections.recv())
        .await
        .is_err()
    {
        kill.cancel();
        let _ = connections.recv().await;
    }
}

/// The listener task's state sender. Reports [`TargetState::Stopped`] when
/// dropped, so the target stops counting as serving however the task ends:
/// by returning, by panicking, or by being dropped with its runtime, even
/// before it first ran.
struct StoppedOnDrop(watch::Sender<TargetState>);

impl Drop for StoppedOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(TargetState::Stopped);
    }
}

/// The accepted connections of one listener, with `TCP_NODELAY` set. Ends,
/// dropping the listener, as soon as `stop` resolves. After an accept error
/// that is not about one connection it accepts nothing for
/// [`ACCEPT_BACKOFF`].
struct Incoming {
    listener: Option<TcpListener>,
    stop: Pin<Box<WaitForCancellationFutureOwned>>,
    kill: CancellationToken,
    alive: mpsc::Sender<()>,
    backoff: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl tokio_stream::Stream for Incoming {
    type Item = io::Result<Connection>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.listener.is_none() {
            return Poll::Ready(None);
        }
        if this.stop.as_mut().poll(cx).is_ready() {
            this.listener = None;
            return Poll::Ready(None);
        }
        if let Some(backoff) = this.backoff.as_mut() {
            if backoff.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.backoff = None;
        }
        let Some(listener) = this.listener.as_ref() else {
            return Poll::Ready(None);
        };
        match listener.poll_accept(cx) {
            Poll::Ready(Ok((io, _))) => {
                if let Err(e) = io.set_nodelay(true) {
                    tracing::debug!(error = %e, "gnmi_target could not set TCP_NODELAY");
                }
                Poll::Ready(Some(Ok(Connection {
                    io,
                    kill: this.kill.clone(),
                    killed: Box::pin(this.kill.clone().cancelled_owned()),
                    _alive: this.alive.clone(),
                })))
            }
            Poll::Ready(Err(e)) if is_connection_error(&e) => {
                tracing::debug!(error = %e, "gnmi_target could not accept a connection");
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    backoff = ?ACCEPT_BACKOFF,
                    "gnmi_target could not accept a connection"
                );
                this.backoff = Some(Box::pin(tokio::time::sleep(ACCEPT_BACKOFF)));
                Poll::Ready(Some(Err(e)))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// An accepted TCP connection that reads end-of-stream, and refuses writes,
/// once `kill` is cancelled.
struct Connection {
    io: TcpStream,
    kill: CancellationToken,
    killed: Pin<Box<WaitForCancellationFutureOwned>>,
    _alive: mpsc::Sender<()>,
}

impl Connection {
    /// Whether `kill` is cancelled; otherwise registers `cx` to be woken
    /// when it is.
    fn is_killed(&mut self, cx: &mut Context<'_>) -> bool {
        self.kill.is_cancelled() || self.killed.as_mut().poll(cx).is_ready()
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.is_killed(cx) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.is_killed(cx) {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.is_killed(cx) {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

impl Connected for Connection {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.io.connect_info()
    }
}

fn lock(latest: &Mutex<Bytes>) -> std::sync::MutexGuard<'_, Bytes> {
    latest.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::*;

    /// The owner in tests that use one; no launch in a test process reaches it.
    const OWNER: Owner = Owner(u64::MAX);
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
    use crate::encoder::Encoder;
    use crate::model::metric::{Labels, MetricEvent};
    use crate::sink::gnmi::GnmiTargetConfig;

    async fn bind(subscriber_buffer: usize) -> Arc<GnmiTarget> {
        let mut cfg = GnmiTargetConfig::new("127.0.0.1:0");
        cfg.subscriber_buffer = Some(subscriber_buffer);
        GnmiTarget::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    /// `/interfaces/interface[name=<if_name>]/<leaf>`.
    fn path(if_name: &str, leaf: &str) -> PathKey {
        PathKey::new(proto::Path {
            origin: String::new(),
            elem: vec![
                proto::PathElem {
                    name: "interfaces".to_string(),
                    key: BTreeMap::new(),
                },
                proto::PathElem {
                    name: "interface".to_string(),
                    key: BTreeMap::from([("name".to_string(), if_name.to_string())]),
                },
                proto::PathElem {
                    name: leaf.to_string(),
                    key: BTreeMap::new(),
                },
            ],
            target: String::new(),
        })
    }

    /// One encoded notification for `labels`, through a gnmi encoder that
    /// places `ifName` and drops `job`.
    fn encoded(labels: &[(&str, &str)], value: f64) -> Vec<u8> {
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            origin: "openconfig".to_string(),
            path: Some("/interfaces/interface[name={ifName}]/{name}".to_string()),
            drop_labels: vec!["job".to_string()],
            ..Default::default()
        })
        .expect("valid encoder");
        let event = MetricEvent::new(
            "in_octets".to_string(),
            value,
            Labels::from_pairs(labels).expect("valid labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        buf
    }

    #[tokio::test]
    async fn publish_allocates_id_once_per_series() {
        let target = bind(16).await;
        let a = path("Gi0", "in-octets");
        let b = path("Gi1", "in-octets");

        let (first, _) = target.publish(OWNER, None, &a, b"one").expect("one owner");
        let (again, _) = target
            .publish(OWNER, Some(first), &a, b"two")
            .expect("one owner");
        let (other, _) = target
            .publish(OWNER, None, &b, b"three")
            .expect("one owner");

        assert_eq!(first, again, "the same path keeps its id");
        assert_ne!(first, other, "a different path is a different series");
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
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, b"value")
            .expect("one owner");
        let started = Instant::now();
        let mut published = 0usize;
        for _ in 0..10_000 {
            let (_, receivers) = target
                .publish(OWNER, Some(id), &series, b"value")
                .expect("one owner");
            assert!(!receivers);
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
        let series = path("Gi0", "in-octets");
        let (id, receivers) = target
            .publish(OWNER, None, &series, b"value")
            .expect("one owner");
        assert!(receivers, "the receiver counts as subscribed");
        for _ in 0..1_000 {
            target
                .publish(OWNER, Some(id), &series, b"value")
                .expect("one owner");
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
        let buf = encoded(&[("device", "rtr1"), ("ifName", "Gi0")], 1.0);
        let key = PathKey::decode(&buf).expect("a notification with an update");

        let target = bind(16).await;
        target.publish(OWNER, None, &key, &buf).expect("one owner");
        let snapshot = target.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].path.to_string(),
            "openconfig:/interfaces/interface[name=Gi0]/in-octets"
        );
        assert_eq!(snapshot[0].path.target, "rtr1");
    }

    /// Label sets that render to one path are one series, holding the
    /// latest value; a different path is another series.
    #[tokio::test]
    async fn series_identity_is_the_rendered_path() {
        let target = bind(16).await;
        let mut last = Vec::new();
        for (job, value) in [("a", 1.0), ("b", 2.0), ("c", 3.0)] {
            last = encoded(&[("ifName", "Gi0"), ("job", job)], value);
            let key = PathKey::decode(&last).expect("decodes");
            target.publish(OWNER, None, &key, &last).expect("one owner");
        }
        assert_eq!(target.series_count(), 1, "`job` is dropped from the path");
        let snapshot = target.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].latest.as_ref(), last.as_slice());

        let other = encoded(&[("ifName", "Gi1"), ("job", "a")], 4.0);
        target
            .publish(
                OWNER,
                None,
                &PathKey::decode(&other).expect("decodes"),
                &other,
            )
            .expect("one owner");
        assert_eq!(target.series_count(), 2);
    }

    /// A cached id that was freed and handed to another path does not
    /// write into that path's series: the publish is resolved by its own
    /// path instead.
    #[tokio::test]
    async fn a_reused_id_does_not_write_into_another_path() {
        let target = bind(16).await;
        let (old_path, new_path) = (path("Gi0", "in-octets"), path("Gi1", "in-octets"));
        let (stale, _) = target
            .publish(OWNER, None, &old_path, b"old")
            .expect("one owner");
        target.release(OWNER, [stale]);
        target.write().next_id = stale;
        let (reused, _) = target
            .publish(OWNER, None, &new_path, b"new")
            .expect("one owner");
        assert_eq!(reused, stale, "the premise: the id was handed out again");

        let (resolved, _) = target
            .publish(OWNER, Some(stale), &old_path, b"old again")
            .expect("one owner");
        assert_ne!(resolved, stale);
        let mut latest: Vec<Bytes> = target.snapshot().into_iter().map(|s| s.latest).collect();
        latest.sort();
        assert_eq!(
            latest,
            [Bytes::from_static(b"new"), Bytes::from_static(b"old again")]
        );
    }

    /// Only a series' owner publishes it, by path or by id; another launch is
    /// refused, the owner's own second first-publish of the path is an upsert
    /// that counts a second publisher, and the series leaves when the last
    /// of its publishers is released.
    #[tokio::test]
    async fn a_series_belongs_to_its_owner() {
        let target = bind(16).await;
        let (owner, other) = (Owner::from(LaunchId::next()), Owner::from(LaunchId::next()));
        assert_ne!(owner, other, "each launch is its own owner");
        let shared = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(owner, None, &shared, b"a")
            .expect("new path");
        let (same, _) = target
            .publish(owner, None, &shared, b"b")
            .expect("own path");
        assert_eq!(
            id, same,
            "the owner's second publish of a path is an upsert"
        );

        let err = target
            .publish(other, None, &shared, b"c")
            .expect_err("another owner's path is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("interface[name=Gi0]/in-octets"), "got {msg}");
        assert!(msg.contains(&target.local_addr().to_string()), "got {msg}");
        let err = target
            .publish(other, Some(id), &shared, b"c")
            .expect_err("another owner's id is refused too");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert_eq!(target.snapshot()[0].latest, Bytes::from_static(b"b"));

        let (own, _) = target
            .publish(other, None, &path("Gi1", "in-octets"), b"d")
            .expect("a free path");
        assert_ne!(own, id);
        assert_eq!(target.series_count(), 2);

        target.release(other, [id]);
        assert_eq!(
            target.series_count(),
            2,
            "another owner's release removes nothing"
        );
        target.release(owner, [id]);
        assert_eq!(target.series_count(), 2, "one publisher of it is left");
        target.release(owner, [id]);
        assert_eq!(target.series_count(), 1, "the last publisher took it");
        target.release(owner, [id]);
        assert_eq!(target.series_count(), 1, "an unknown id is ignored");
        let (again, _) = target
            .publish(other, None, &shared, b"e")
            .expect("the path is free once its publishers left");
        target.release(other, [own, again]);
        assert_eq!(target.series_count(), 0);
    }

    #[test]
    fn decode_refuses_bytes_that_are_not_a_notification() {
        let err = PathKey::decode(b"\xff\xff").expect_err("not protobuf");
        assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
        let empty = proto::Notification::default().encode_to_vec();
        let err = PathKey::decode(&empty).expect_err("no update");
        assert!(err.to_string().contains("no update path"), "got {err}");
    }

    /// A client that connects and never speaks does not hold the port or
    /// the teardown: the port re-binds at once and the connection is
    /// closed within the drain grace.
    #[tokio::test]
    async fn silent_tcp_connection_does_not_hold_teardown() {
        use tokio::io::AsyncReadExt;

        let target = bind(16).await;
        let addr = target.local_addr();
        let mut silent = TcpStream::connect(addr)
            .await
            .expect("the port accepts while the target lives");
        // The server's HTTP/2 preface proves the connection was accepted.
        let mut preface = [0u8; 1];
        tokio::time::timeout(Duration::from_secs(1), silent.read_exact(&mut preface))
            .await
            .expect("the server answers an accepted connection")
            .expect("the server writes its preface");

        let stopped = target.wait();
        let mut state = target.state();
        let started = Instant::now();
        drop(target);
        tokio::time::timeout(
            Duration::from_secs(1),
            state.wait_for(|s| *s != TargetState::Listening),
        )
        .await
        .expect("the listening socket closes at once")
        .expect("the listener task reports its state");
        let settings = GnmiTargetConfig::new(addr.to_string())
            .settings()
            .expect("valid settings");
        let rebound = GnmiTarget::bind(settings)
            .await
            .expect("the port re-binds while the silent client is still connected");
        assert_eq!(rebound.local_addr(), addr);
        drop(rebound);

        tokio::time::timeout(Duration::from_secs(2), stopped)
            .await
            .expect("teardown completes within 2 s");
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), silent.read_to_end(&mut rest))
            .await
            .expect("the silent connection is closed, not left open")
            .ok();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "teardown took {:?}",
            started.elapsed()
        );
    }

    /// Ids handed out after a wrap skip the ids of series still in the table.
    #[tokio::test]
    async fn ids_after_a_wrap_skip_live_series() {
        let target = bind(16).await;
        let (first, _) = target
            .publish(OWNER, None, &path("Gi0", "in-octets"), b"a")
            .expect("one owner");
        assert_eq!(first, 0);
        target.write().next_id = SeriesId::MAX;
        let (last, _) = target
            .publish(OWNER, None, &path("Gi1", "in-octets"), b"b")
            .expect("one owner");
        assert_eq!(last, SeriesId::MAX);
        let (wrapped, _) = target
            .publish(OWNER, None, &path("Gi2", "in-octets"), b"c")
            .expect("one owner");
        assert_ne!(wrapped, first, "id 0 is still in use");
        assert_eq!(wrapped, 1);
        assert_eq!(target.series_count(), 3);
        let latest: Vec<Bytes> = {
            let mut snapshot = target.snapshot();
            snapshot.sort_by_key(|s| s.series_id);
            snapshot.into_iter().map(|s| s.latest).collect()
        };
        assert_eq!(
            latest,
            [
                Bytes::from_static(b"a"),
                Bytes::from_static(b"c"),
                Bytes::from_static(b"b")
            ]
        );
    }

    /// Accepted connections have `TCP_NODELAY` set and keep vectored writes.
    #[tokio::test]
    async fn accepted_connections_are_nodelay_and_vectored() {
        use tokio_stream::StreamExt;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("bound address");
        let token = CancellationToken::new();
        let (alive, _connections) = mpsc::channel::<()>(1);
        let mut incoming = Incoming {
            listener: Some(listener),
            stop: Box::pin(token.clone().cancelled_owned()),
            kill: CancellationToken::new(),
            alive,
            backoff: None,
        };
        let _client = TcpStream::connect(addr).await.expect("connect");
        let connection = tokio::time::timeout(Duration::from_secs(1), incoming.next())
            .await
            .expect("a connection is accepted")
            .expect("the stream has not ended")
            .expect("the accept succeeded");
        assert!(connection.io.nodelay().expect("read TCP_NODELAY"));
        assert_eq!(
            connection.is_write_vectored(),
            connection.io.is_write_vectored()
        );
        assert!(
            connection.is_write_vectored(),
            "a TcpStream writes vectored"
        );

        token.cancel();
        assert!(
            incoming.next().await.is_none(),
            "the stream ends once stopped"
        );
        assert!(incoming.listener.is_none(), "and drops its listener");
    }

    /// An overwrite replaces the latest value of a served path without
    /// taking ownership, and refuses a path nothing serves.
    #[tokio::test]
    async fn overwrite_replaces_only_a_served_path() {
        let target = bind(16).await;
        let served = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &served, b"scenario")
            .expect("one owner");

        assert_eq!(target.overwrite(&served, b"posted"), Some(id));
        assert_eq!(target.snapshot()[0].latest, Bytes::from_static(b"posted"));
        assert_eq!(target.overwrite(&path("Gi9", "in-octets"), b"posted"), None);
        assert_eq!(target.series_count(), 1, "an unknown path is not added");

        target.release(OWNER, [id]);
        assert_eq!(target.series_count(), 0, "the overwrite took no ownership");
    }

    /// Concurrent publishers of one path: after every round of four
    /// simultaneous publishes, the last update a subscriber received is the
    /// value the table holds.
    #[test]
    fn concurrent_publishers_broadcast_in_table_order() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Barrier;

        const WRITERS: usize = 4;
        const ROUNDS: usize = 2_000;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let target = rt.block_on(bind(1 << 16));
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, b"seed")
            .expect("one owner");
        let barrier = Barrier::new(WRITERS);
        let (checked, mismatches) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let (target, series, barrier) = (&target, &series, &barrier);
                let (checked, mismatches) = (&checked, &mismatches);
                scope.spawn(move || {
                    let mut receiver = (writer == 0).then(|| target.subscribe());
                    for round in 0..ROUNDS {
                        barrier.wait();
                        let value = format!("{round}-{writer}");
                        target
                            .publish(OWNER, Some(id), series, value.as_bytes())
                            .expect("one owner");
                        barrier.wait();
                        if let Some(receiver) = receiver.as_mut() {
                            let mut last = None;
                            while let Ok(update) = receiver.try_recv() {
                                last = Some(update.bytes);
                            }
                            if last != Some(target.snapshot()[0].latest.clone()) {
                                mismatches.fetch_add(1, Ordering::Relaxed);
                            }
                            checked.fetch_add(1, Ordering::Relaxed);
                        }
                        barrier.wait();
                    }
                });
            }
        });
        assert_eq!(
            checked.load(Ordering::Relaxed),
            ROUNDS,
            "every round was checked"
        );
        assert_eq!(
            mismatches.load(Ordering::Relaxed),
            0,
            "rounds whose last broadcast was not the stored value"
        );
    }

    /// A subscriber that subscribes and then takes a snapshot while a value
    /// is being published either finds the value in the snapshot or receives
    /// it.
    #[test]
    fn a_subscriber_never_misses_a_concurrent_publish() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Barrier;

        const ROUNDS: usize = 5_000;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let target = rt.block_on(bind(16));
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, b"seed")
            .expect("one owner");
        let barrier = Barrier::new(2);
        let missed = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let (target, series, barrier) = (&target, &series, &barrier);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    barrier.wait();
                    target
                        .publish(OWNER, Some(id), series, round.to_string().as_bytes())
                        .expect("one owner");
                    barrier.wait();
                    barrier.wait();
                }
            });
            for round in 0..ROUNDS {
                barrier.wait();
                let mut receiver = target.subscribe();
                let mut seen = target.snapshot()[0].latest.clone();
                barrier.wait();
                while let Ok(update) = receiver.try_recv() {
                    seen = update.bytes;
                }
                if seen != round.to_string().as_bytes() {
                    missed.fetch_add(1, Ordering::Relaxed);
                }
                drop(receiver);
                barrier.wait();
            }
        });
        assert_eq!(target.subscriber_count(), 0, "every round's receiver left");
        assert_eq!(
            missed.load(Ordering::Relaxed),
            0,
            "rounds whose subscriber ended up without the published value"
        );
    }

    /// Only an accept error that is not about one connection makes the
    /// listener back off.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::refused(io::ErrorKind::ConnectionRefused, true)]
    #[case::aborted(io::ErrorKind::ConnectionAborted, true)]
    #[case::reset(io::ErrorKind::ConnectionReset, true)]
    #[case::out_of_memory(io::ErrorKind::OutOfMemory, false)]
    #[case::other(io::ErrorKind::Other, false)]
    fn only_a_connection_error_skips_the_accept_backoff(
        #[case] kind: io::ErrorKind,
        #[case] per_connection: bool,
    ) {
        assert_eq!(is_connection_error(&io::Error::from(kind)), per_connection);
    }

    /// The descriptor limits are not per-connection errors, so the listener
    /// backs off from them.
    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    #[case::emfile(24)]
    #[case::enfile(23)]
    #[case::enobufs(105)]
    #[case::enomem(12)]
    fn resource_errors_take_the_accept_backoff(#[case] errno: i32) {
        assert!(!is_connection_error(&io::Error::from_raw_os_error(errno)));
    }

    /// The bound address is logged at warn when port 0 was asked for, so a
    /// caller at the default log level learns which port it got.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn an_ephemeral_port_is_logged_at_warn() {
        let target = bind(16).await;
        let addr = target.local_addr().to_string();
        assert!(logs_contain("gnmi_target listening on an ephemeral port"));
        assert!(logs_contain(&addr));
        assert!(logs_contain("WARN"));
    }
}
