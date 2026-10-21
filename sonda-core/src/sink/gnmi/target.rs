//! The shared state behind one gNMI listen address.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io;
use std::net::SocketAddr;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tonic::transport::server::{Connected, TcpConnectInfo};
use tonic::transport::Server;

use super::notification::{self, value_span, ValueKind};
use super::service::GnmiService;
use super::{error_chain, TargetSettings};
use crate::encoder::gnmi::{encode_delete, proto};
use crate::schedule::LaunchId;
use crate::{ConfigError, SondaError};

/// How long a connection still open when its target stops may stay open
/// before the target closes it.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// How long an HTTP/2 connection may be silent before the server pings it.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long the server waits for a ping's answer before it closes the
/// connection, which ends that connection's subscriptions.
pub(crate) const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a TLS target waits for a connection to finish its TLS
/// handshake before it closes the connection.
pub(crate) const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A changed value for one series, as broadcast to subscribers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SeriesUpdate {
    /// The series the value belongs to.
    pub series_id: SeriesId,
    /// The kind of value the series carries, fixed at its first publish.
    pub kind: ValueKind,
    /// The series' version with this value: 1 for its first value, one more
    /// for each change. Set under the series' lock, so one series' updates
    /// are broadcast in the order of their versions.
    pub version: u64,
    /// One encoded `gnmi.Notification`.
    pub bytes: Bytes,
}

/// What the target broadcasts to its subscribers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(crate) enum SeriesEvent {
    /// A series' value changed.
    Changed(SeriesUpdate),
    /// A series left the table. `bytes` is one encoded `gnmi.Notification`
    /// deleting its path, from [`delete_notification`].
    Removed { series_id: SeriesId, bytes: Bytes },
}

/// One encoded `gnmi.Notification` deleting the series at `path`, stamped
/// with the current time (0 before the epoch).
pub(crate) fn delete_notification(path: &proto::Path) -> Bytes {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        });
    let mut buf = Vec::new();
    encode_delete(&mut buf, path, timestamp);
    Bytes::from(buf)
}

/// What one publish did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Published {
    /// Whether any broadcast receiver was subscribed.
    pub(crate) receivers: bool,
    /// Whether the value changed: its encoded `TypedValue` bytes differ from
    /// the series' previous ones. The notification's timestamp is not part of
    /// the value. A series' first publish is a change.
    pub(crate) changed: bool,
}

/// [`SondaError::Sink`]: `encoded` is not a gnmi notification with an update
/// that has a path and a value.
fn not_a_notification(detail: impl std::fmt::Display) -> SondaError {
    SondaError::Sink(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "gnmi_target received bytes that are not a gnmi notification with an update: {detail}"
        ),
    ))
}

/// The gNMI identity of a series: its full path, with the origin and target
/// name. Two events whose notifications carry one path are one series.
///
/// The key's hash is computed once, when the key is made, and stored with
/// the path: hashing the key writes that one value, so a table holding the
/// key grows without hashing any path again. Keys compare by hash, then by
/// path.
#[derive(Clone, Debug)]
pub(crate) struct PathKey(Arc<Keyed>);

/// A path and its hash.
#[derive(Debug)]
struct Keyed {
    path: proto::Path,
    hash: u64,
}

impl PathKey {
    /// A key for `path`.
    pub(crate) fn new(path: proto::Path) -> Self {
        let mut hasher = std::hash::DefaultHasher::new();
        path.origin.hash(&mut hasher);
        path.target.hash(&mut hasher);
        path.elem.len().hash(&mut hasher);
        for elem in &path.elem {
            elem.name.hash(&mut hasher);
            elem.key.hash(&mut hasher);
        }
        let hash = hasher.finish();
        Self(Arc::new(Keyed { path, hash }))
    }

    /// The path of the first update in an encoded `gnmi.Notification`: the
    /// prefix's origin and target with the update's elements.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when `encoded` is not a notification with an
    /// update.
    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, SondaError> {
        let mut notification = proto::Notification::decode(encoded).map_err(not_a_notification)?;
        let prefix = notification.prefix.take().unwrap_or_default();
        let elem = notification
            .update
            .into_iter()
            .next()
            .and_then(|update| update.path)
            .ok_or_else(|| not_a_notification("no update path"))?
            .elem;
        Ok(Self::new(proto::Path {
            origin: prefix.origin,
            elem,
            target: prefix.target,
        }))
    }

    /// The path.
    pub(crate) fn path(&self) -> &proto::Path {
        &self.0.path
    }

    /// A key for `path` carrying `hash` instead of the path's own.
    #[cfg(test)]
    fn with_hash(path: proto::Path, hash: u64) -> Self {
        Self(Arc::new(Keyed { path, hash }))
    }

    /// Whether `self` and `other` are one key, not just equal ones.
    #[cfg(test)]
    pub(crate) fn is_same_key(&self, other: &PathKey) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for PathKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.hash == other.0.hash && self.0.path == other.0.path)
    }
}

impl Eq for PathKey {}

impl Hash for PathKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash);
    }
}

/// One series as held by the target, returned by [`GnmiTarget::snapshot`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(crate) struct SeriesSnapshot {
    pub(crate) series_id: SeriesId,
    /// The series' full path: the notification prefix's origin and target
    /// with the update's elements. Shared with the table, not copied.
    pub(crate) path: PathKey,
    /// The kind of value the series carries, fixed at its first publish.
    pub(crate) kind: ValueKind,
    /// The series' version with `latest`, as [`SeriesUpdate::version`].
    pub(crate) version: u64,
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
    latest: Mutex<Latest>,
    /// The kind of the value of the series' first publish.
    kind: ValueKind,
    /// The only launch whose sinks may publish the series.
    owner: Owner,
    /// Sinks of `owner` that have published the series; it leaves the table
    /// when the last of them is released.
    publishers: usize,
}

/// A series' latest encoded `gnmi.Notification`, where its value lies in
/// it, the series' version, and the value's JSON_IETF form once a subscriber
/// has asked for it.
#[derive(Default)]
struct Latest {
    bytes: Bytes,
    /// The range of the update's encoded `TypedValue` in `bytes`.
    value: Range<usize>,
    /// How many times the series' value has changed; 0 before its first.
    version: u64,
    /// The value as an encoded `TypedValue` holding `json_ietf_val`, made by
    /// the first [`GnmiTarget::json_ietf_value`] after the value last changed;
    /// `None` until then. A publish that does not change the value keeps it.
    json: Option<Bytes>,
}

impl Latest {
    /// The encoded `TypedValue`, borrowed from `bytes`.
    fn value(&self) -> &[u8] {
        self.bytes.get(self.value.clone()).unwrap_or_default()
    }
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
/// by the series' path, the broadcast that carries changed values and
/// removed series to subscribers, and the gRPC listener.
///
/// Obtained from the [`registry`](super::registry::registry). The listener
/// runs on its own task until the last `Arc<GnmiTarget>` is dropped, which
/// cancels the target's root token: the listening socket closes at once,
/// and connections still open after [`DRAIN_GRACE`] are closed.
///
/// A series is owned by one launch and held while any sink of that launch
/// has published it; it leaves when the last of those sinks drops, and at no
/// other time: a series that stops being updated stays. When a series leaves
/// and anyone is subscribed, a delete for its path is broadcast. A path with
/// no live publisher is free: another
/// launch may take it, and a scenario of the first launch refused it in that
/// window shows why in its stats.
pub struct GnmiTarget {
    local_addr: SocketAddr,
    settings: TargetSettings,
    table: RwLock<SeriesTable>,
    tx: broadcast::Sender<SeriesEvent>,
    /// One permit per subscription the target may serve at once.
    subscribers: Arc<Semaphore>,
    /// The permits `subscribers` started with.
    subscriber_limit: usize,
    token: CancellationToken,
    state: watch::Receiver<TargetState>,
    error: ServeError,
}

impl GnmiTarget {
    /// Bind `settings.listen` and start serving on a spawned task, over TLS
    /// when the settings carry it.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] naming the address when the TLS files cannot
    /// make a server configuration, or the address cannot be bound. Nothing
    /// is bound when TLS fails.
    pub(crate) async fn bind(settings: TargetSettings) -> Result<Arc<Self>, SondaError> {
        let listen = settings.listen;
        let server = server(&settings)?;
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
        // A limit beyond what a semaphore holds is no limit in practice.
        let subscriber_limit = settings.max_subscribers.min(Semaphore::MAX_PERMITS);
        let token = CancellationToken::new();
        let (state_tx, state) = watch::channel(TargetState::Listening);
        let error = ServeError::default();

        Ok(Arc::new_cyclic(|target| {
            let service = GnmiService::new(target.clone());
            tokio::spawn(serve(
                server,
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
                subscribers: Arc::new(Semaphore::new(subscriber_limit)),
                subscriber_limit,
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

    /// Subscriptions the target is serving: the places taken through
    /// [`admit`](Self::admit) and not yet given back.
    pub fn subscriber_count(&self) -> usize {
        self.subscriber_limit - self.subscribers.available_permits()
    }

    /// A place for one more subscription, given back when the permit drops;
    /// `None` when the target already serves `max_subscribers`.
    pub(crate) fn admit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.subscribers).try_acquire_owned().ok()
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

    /// Broadcast updates some receiver has yet to read.
    #[cfg(test)]
    pub(crate) fn broadcast_backlog(&self) -> usize {
        self.tx.len()
    }

    /// The settings the target was bound with.
    pub(crate) fn settings(&self) -> &TargetSettings {
        &self.settings
    }

    /// Upsert `encoded` as the latest notification of a series `owner`
    /// publishes. Returns the series' id and what the publish did.
    ///
    /// With `id` naming a series `owner` holds, that series is updated under
    /// the read lock. Otherwise (no `id`, or one that no longer names this
    /// path's series) this is the publisher's first publish of `path`: the
    /// series is found or created under the write lock, with a new id and
    /// `owner` as its owner, and counts one more publisher. Each publisher is
    /// [released](Self::release) once; the series leaves with the last.
    ///
    /// Synchronous; `broadcast::Sender::send` never blocks. Locates the value
    /// in `encoded`, copies `encoded` once and compares the value with the
    /// series' previous one; only a changed value is broadcast. The update is
    /// broadcast while the series is still locked, so concurrent publishes of
    /// one path reach subscribers in the order the table stores them.
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when `encoded` is not a gnmi notification whose
    /// update has a value; [`SondaError::Config`] naming the path and the
    /// listen address when another launch owns the series for `path`.
    pub(crate) fn publish(
        &self,
        owner: Owner,
        id: Option<SeriesId>,
        path: &PathKey,
        encoded: &[u8],
    ) -> Result<(SeriesId, Published), SondaError> {
        let (latest, kind) = latest(encoded)?;
        self.store(owner, id, path, latest, kind)
    }

    /// Replace the latest notification of the series already in the table
    /// for `path`, whoever owns it, taking no ownership. Returns its id, or
    /// `None` when no series has that path. Synchronous, and compared and
    /// broadcast like [`publish`](Self::publish).
    ///
    /// # Errors
    ///
    /// [`SondaError::Sink`] when `encoded` is not a gnmi notification whose
    /// update has a value.
    pub(crate) fn overwrite(
        &self,
        path: &PathKey,
        encoded: &[u8],
    ) -> Result<Option<SeriesId>, SondaError> {
        let (latest, _) = latest(encoded)?;
        let table = self.read();
        let Some((id, slot)) = table
            .ids
            .get(path)
            .and_then(|id| table.slots.get(id).map(|slot| (*id, slot)))
        else {
            return Ok(None);
        };
        self.replace(id, slot.kind, &mut lock(&slot.latest), latest);
        Ok(Some(id))
    }

    /// How many publishers series `id` counts.
    #[cfg(test)]
    pub(crate) fn publishers(&self, id: SeriesId) -> Option<usize> {
        self.read().slots.get(&id).map(|slot| slot.publishers)
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
    ///
    /// When a series leaves and a receiver exists, its delete is encoded once
    /// and broadcast as [`SeriesEvent::Removed`] while the table's write lock
    /// is held. Every publish holds the read lock, so no receiver gets a
    /// value for the series after its delete. With no receiver nothing is
    /// encoded.
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
                    if self.tx.receiver_count() > 0 {
                        // An error means every receiver has gone.
                        let _ = self.tx.send(SeriesEvent::Removed {
                            series_id: id,
                            bytes: delete_notification(slot.path.path()),
                        });
                    }
                }
            }
        }
    }

    /// Series `id`'s value at `version` as an encoded `TypedValue` holding
    /// `json_ietf_val` ([`notification::json_ietf_value`]), where `latest` is
    /// the series' encoded notification at `version`.
    ///
    /// While `version` is the series' current version, the value is made once
    /// and kept with the series, under the series' lock: the first call after
    /// a change makes it, and every later call returns the same `Bytes` until
    /// the value changes again. When the series has moved past `version`, the
    /// value is made from `latest` and not kept. Publishers never make it.
    ///
    /// `None` when no series has `id`, or the value has no JSON_IETF form.
    pub(crate) fn json_ietf_value(
        &self,
        id: SeriesId,
        version: u64,
        latest: &[u8],
    ) -> Option<Bytes> {
        let table = self.read();
        let slot = table.slots.get(&id)?;
        let mut current = lock(&slot.latest);
        if current.version != version {
            drop(current);
            drop(table);
            return self.made_json_ietf(latest);
        }
        if let Some(json) = &current.json {
            return Some(json.clone());
        }
        let json = self.made_json_ietf(&current.bytes)?;
        current.json = Some(json.clone());
        Some(json)
    }

    /// The notification `latest`, series `id`'s at `version`, with its value
    /// as JSON_IETF: [`notification::with_value`] of
    /// [`json_ietf_value`](Self::json_ietf_value). The timestamp, prefix and
    /// path are `latest`'s.
    ///
    /// `None` when no series has `id`, or the value has no JSON_IETF form.
    pub(crate) fn json_ietf_of(&self, id: SeriesId, version: u64, latest: &[u8]) -> Option<Bytes> {
        notification::with_value(latest, &self.json_ietf_value(id, version, latest)?)
    }

    /// [`notification::json_ietf_value`] of `encoded`, counted for tests.
    fn made_json_ietf(&self, encoded: &[u8]) -> Option<Bytes> {
        #[cfg(test)]
        self.settings
            .probe
            .json_ietf_values
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        notification::json_ietf_value(encoded)
    }

    /// A receiver for every changed value and removed series from now on.
    /// Subscribe before taking a [`snapshot`](Self::snapshot): every change
    /// the snapshot does not hold then arrives on the receiver.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SeriesEvent> {
        self.tx.subscribe()
    }

    /// Every series with its path and latest value, in no particular order.
    /// Each entry shares the table's path and value; nothing is deep-copied.
    pub(crate) fn snapshot(&self) -> Vec<SeriesSnapshot> {
        self.read()
            .slots
            .iter()
            .map(|(id, slot)| {
                let latest = lock(&slot.latest);
                SeriesSnapshot {
                    series_id: *id,
                    path: slot.path.clone(),
                    kind: slot.kind,
                    version: latest.version,
                    latest: latest.bytes.clone(),
                }
            })
            .collect()
    }

    fn store(
        &self,
        owner: Owner,
        id: Option<SeriesId>,
        path: &PathKey,
        latest: Latest,
        kind: ValueKind,
    ) -> Result<(SeriesId, Published), SondaError> {
        if let Some(id) = id {
            let table = self.read();
            // A cached id names this path's series unless the id was freed
            // and handed to another path since.
            if let Some(slot) = table
                .slots
                .get(&id)
                .filter(|slot| slot.owner == owner && slot.path == *path)
            {
                let published = self.replace(id, slot.kind, &mut lock(&slot.latest), latest);
                return Ok((id, published));
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
            latest: Mutex::new(Latest::default()),
            kind,
            owner,
            publishers: 0,
        });
        // Reaching here is a publisher's first publish of this path: with no
        // id, or with a cached id that no longer names its series.
        slot.publishers += 1;
        let kind = slot.kind;
        let current = slot
            .latest
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let published = self.replace(id, kind, current, latest);
        Ok((id, published))
    }

    /// Store `latest` in `current` and, when its value differs from the one
    /// it replaces, raise the series' version by one and, when anyone is
    /// subscribed, broadcast it with that version. A series' first value
    /// differs from the empty one its slot starts with. The caller holds the
    /// series' lock, so the store, the version, the check for receivers and
    /// the broadcast are one step: a subscriber that takes a snapshot after
    /// subscribing sees the stored value or receives it, and a snapshot's
    /// version is that of the value it holds.
    fn replace(
        &self,
        id: SeriesId,
        kind: ValueKind,
        current: &mut Latest,
        latest: Latest,
    ) -> Published {
        let changed = current.value() != latest.value();
        let version = current.version + u64::from(changed);
        let receivers = self.tx.receiver_count() > 0;
        // A clone shares the buffer but allocates its refcount on first use,
        // so it is made only when someone will receive it.
        let update = (changed && receivers).then(|| latest.bytes.clone());
        let json = if changed { None } else { current.json.take() };
        *current = Latest {
            version,
            json,
            ..latest
        };
        if let Some(bytes) = update {
            // An error means every receiver has gone; there is nobody to tell.
            let _ = self.tx.send(SeriesEvent::Changed(SeriesUpdate {
                series_id: id,
                kind,
                version,
                bytes,
            }));
        }
        Published { receivers, changed }
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

/// The server a target with `settings` runs: a connection silent for the
/// keepalive interval is pinged and closed when the ping goes unanswered for
/// the keepalive timeout, and every connection is TLS when the settings carry
/// TLS files, closed when its handshake has not finished within the TLS
/// handshake timeout.
///
/// # Errors
///
/// [`SondaError::Sink`] naming the listen address when the TLS files cannot
/// make a server configuration.
fn server(settings: &TargetSettings) -> Result<Server, SondaError> {
    let server = Server::builder()
        .http2_keepalive_interval(Some(settings.keepalive_interval))
        .http2_keepalive_timeout(Some(settings.keepalive_timeout));
    match &settings.tls {
        None => Ok(server),
        Some(tls) => server
            .tls_config(tls.server_config().timeout(settings.tls_handshake_timeout))
            .map_err(|e| {
                SondaError::Sink(io::Error::other(format!(
                    "gnmi_target cannot serve TLS on {}: {}",
                    settings.listen,
                    error_chain(&e)
                )))
            }),
    }
}

/// Serve `service` with `server` on `listener` until `token` is cancelled,
/// then close the listening socket, give open connections [`DRAIN_GRACE`]
/// to finish, and close the rest. Reports each step on `state`.
async fn serve(
    server: Server,
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
    let served = server.serve_with_incoming(service, incoming).await;
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

fn lock(latest: &Mutex<Latest>) -> std::sync::MutexGuard<'_, Latest> {
    latest.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A copy of `encoded` with its value located, and the value's kind.
///
/// # Errors
///
/// [`SondaError::Sink`] when `encoded` is not a gnmi notification whose
/// update has a value.
fn latest(encoded: &[u8]) -> Result<(Latest, ValueKind), SondaError> {
    let (value, kind) = value_span(encoded).ok_or_else(|| not_a_notification("no update value"))?;
    let latest = Latest {
        bytes: Bytes::copy_from_slice(encoded),
        value,
        // Set when the value is stored.
        version: 0,
        json: None,
    };
    Ok((latest, kind))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::*;

    /// The owner in tests that use one; no launch in a test process reaches it.
    const OWNER: Owner = Owner(u64::MAX);

    /// The update `event` carries; a removal fails the test.
    fn as_change(event: SeriesEvent) -> SeriesUpdate {
        match event {
            SeriesEvent::Changed(update) => update,
            other => panic!("a change, got {other:?}"),
        }
    }
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig, GnmiValueType};
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
        let event = MetricEvent::new(
            "in_octets".to_string(),
            value,
            Labels::from_pairs(labels).expect("valid labels"),
        )
        .expect("valid event");
        encode(&event, GnmiValueType::Double)
    }

    /// `event` encoded with `in_octets` typed as `value_type`, through a
    /// gnmi encoder that places `ifName` and drops `job`.
    fn encode(event: &MetricEvent, value_type: GnmiValueType) -> Vec<u8> {
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            origin: "openconfig".to_string(),
            path: Some("/interfaces/interface[name={ifName}]/{name}".to_string()),
            drop_labels: vec!["job".to_string()],
            values: std::collections::HashMap::from([("in_octets".to_string(), value_type)]),
            ..Default::default()
        })
        .expect("valid encoder");
        let mut buf = Vec::new();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        buf
    }

    /// An encoded notification carrying `value`, for publishing under any
    /// path.
    fn note(value: f64) -> Bytes {
        Bytes::from(encoded(&[("ifName", "Gi0")], value))
    }

    /// `value` encoded as a double, stamped `nanos` after the epoch.
    fn note_at(value: f64, nanos: u64) -> Bytes {
        let event = MetricEvent::with_timestamp(
            "in_octets".to_string(),
            value,
            Labels::from_pairs(&[("ifName", "Gi0")]).expect("valid labels"),
            std::time::UNIX_EPOCH + Duration::from_nanos(nanos),
        )
        .expect("valid event");
        Bytes::from(encode(&event, GnmiValueType::Double))
    }

    #[tokio::test]
    async fn publish_allocates_id_once_per_series() {
        let target = bind(16).await;
        let a = path("Gi0", "in-octets");
        let b = path("Gi1", "in-octets");

        let (one, two, three) = (note(1.0), note(2.0), note(3.0));
        let (first, _) = target.publish(OWNER, None, &a, &one).expect("one owner");
        let (again, _) = target
            .publish(OWNER, Some(first), &a, &two)
            .expect("one owner");
        let (other, _) = target.publish(OWNER, None, &b, &three).expect("one owner");

        assert_eq!(first, again, "the same path keeps its id");
        assert_ne!(first, other, "a different path is a different series");
        assert_eq!(target.series_count(), 2);
        let latest: Vec<Bytes> = {
            let mut snapshot = target.snapshot();
            snapshot.sort_by_key(|s| s.series_id);
            snapshot.into_iter().map(|s| s.latest).collect()
        };
        assert_eq!(latest, [two, three]);
    }

    #[tokio::test]
    async fn publish_never_blocks_without_receivers() {
        let target = bind(4).await;
        let series = path("Gi0", "in-octets");
        let value = note(1.0);
        let (id, _) = target
            .publish(OWNER, None, &series, &value)
            .expect("one owner");
        let started = Instant::now();
        let mut published = 0usize;
        for _ in 0..10_000 {
            let (_, done) = target
                .publish(OWNER, Some(id), &series, &value)
                .expect("one owner");
            assert!(!done.receivers);
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
        let (id, done) = target
            .publish(OWNER, None, &series, &note(-1.0))
            .expect("one owner");
        assert!(done.receivers, "the receiver counts as subscribed");
        // Each value differs from the last, so each is broadcast.
        let values: Vec<Bytes> = (0..1_000).map(|i| note(f64::from(i))).collect();
        for value in &values {
            target
                .publish(OWNER, Some(id), &series, value)
                .expect("one owner");
        }
        match receiver.recv().await {
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                assert!(skipped > 0, "a lag reports what it skipped");
            }
            other => panic!("a receiver that never read must have lagged, got {other:?}"),
        }
    }

    fn hash_of(key: &PathKey) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Two keys for one path are equal and hash alike, however each was
    /// made; keys whose hashes match but whose paths differ are not equal.
    #[test]
    fn path_keys_compare_by_hash_then_path() {
        let a = path("Gi0", "in-octets");
        let same = path("Gi0", "in-octets");
        assert!(!a.is_same_key(&same), "two keys, not one");
        assert_eq!(a, same);
        assert_eq!(hash_of(&a), hash_of(&same));

        let buf = encoded(&[("ifName", "Gi0")], 1.0);
        let decoded = PathKey::decode(&buf).expect("a notification with an update");
        let built = PathKey::new(decoded.path().clone());
        assert_eq!(decoded, built);
        assert_eq!(hash_of(&decoded), hash_of(&built));

        let other = path("Gi1", "in-octets");
        assert_ne!(a, other);
        let collided = PathKey::with_hash(other.path().clone(), a.0.hash);
        assert_eq!(hash_of(&collided), hash_of(&a), "the forced hash is used");
        assert_ne!(collided, a, "an equal hash is not an equal path");
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
            snapshot[0].path.path().to_string(),
            "openconfig:/interfaces/interface[name=Gi0]/in-octets"
        );
        assert_eq!(snapshot[0].path.path().target, "rtr1");
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
        let (old, new, old_again) = (note(1.0), note(2.0), note(3.0));
        let (stale, _) = target
            .publish(OWNER, None, &old_path, &old)
            .expect("one owner");
        target.release(OWNER, [stale]);
        target.write().next_id = stale;
        let (reused, _) = target
            .publish(OWNER, None, &new_path, &new)
            .expect("one owner");
        assert_eq!(reused, stale, "the premise: the id was handed out again");

        let (resolved, _) = target
            .publish(OWNER, Some(stale), &old_path, &old_again)
            .expect("one owner");
        assert_ne!(resolved, stale);
        assert_eq!(
            target.publishers(reused),
            Some(1),
            "the other path's count is untouched"
        );
        assert_eq!(
            target.publishers(resolved),
            Some(1),
            "the re-published series counts its publisher"
        );
        let mut latest: Vec<Bytes> = target.snapshot().into_iter().map(|s| s.latest).collect();
        latest.sort();
        let mut expected = [new, old_again];
        expected.sort();
        assert_eq!(latest, expected);
        target.release(OWNER, [resolved, reused]);
        assert_eq!(
            target.series_count(),
            0,
            "both series leave with their publisher"
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
        let (a, b, c) = (note(1.0), note(2.0), note(3.0));
        let (id, _) = target.publish(owner, None, &shared, &a).expect("new path");
        let (same, _) = target.publish(owner, None, &shared, &b).expect("own path");
        assert_eq!(
            id, same,
            "the owner's second publish of a path is an upsert"
        );

        let err = target
            .publish(other, None, &shared, &c)
            .expect_err("another owner's path is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("interface[name=Gi0]/in-octets"), "got {msg}");
        assert!(msg.contains(&target.local_addr().to_string()), "got {msg}");
        let err = target
            .publish(other, Some(id), &shared, &c)
            .expect_err("another owner's id is refused too");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert_eq!(target.snapshot()[0].latest, b);

        let (own, _) = target
            .publish(other, None, &path("Gi1", "in-octets"), &note(4.0))
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
            .publish(other, None, &shared, &note(5.0))
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

    /// Two publishes of one value are one change: the second, stamped later,
    /// is stored but not broadcast. A different value is a change and is
    /// broadcast once.
    #[tokio::test]
    async fn only_a_changed_value_is_broadcast() {
        let target = bind(16).await;
        let mut receiver = target.subscribe();
        let series = path("Gi0", "in-octets");
        let first = note_at(1.0, 1_000);
        let (id, done) = target
            .publish(OWNER, None, &series, &first)
            .expect("one owner");
        assert_eq!(
            done,
            Published {
                receivers: true,
                changed: true
            },
            "a series' first publish is a change"
        );
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).bytes,
            first
        );

        let same = note_at(1.0, 2_000);
        assert_ne!(same, first, "the premise: the timestamps differ");
        let (_, done) = target
            .publish(OWNER, Some(id), &series, &same)
            .expect("one owner");
        assert!(done.receivers);
        assert!(!done.changed, "only the timestamp differs");
        assert_eq!(
            receiver.try_recv().map(|e| as_change(e).bytes),
            Err(broadcast::error::TryRecvError::Empty),
            "an unchanged value is not broadcast"
        );
        assert_eq!(target.snapshot()[0].latest, same, "but it is stored");

        let changed = note_at(2.0, 3_000);
        let (_, done) = target
            .publish(OWNER, Some(id), &series, &changed)
            .expect("one owner");
        assert!(done.changed);
        let update = as_change(receiver.try_recv().expect("broadcast"));
        assert_eq!((update.series_id, update.bytes), (id, changed));
        assert_eq!(
            receiver.try_recv().map(|e| as_change(e).bytes),
            Err(broadcast::error::TryRecvError::Empty),
            "one broadcast per change"
        );

        drop(receiver);
        let (_, done) = target
            .publish(OWNER, Some(id), &series, &note_at(3.0, 4_000))
            .expect("one owner");
        assert_eq!(
            done,
            Published {
                receivers: false,
                changed: true
            }
        );
    }

    /// The full path a delete notification names: its prefix's origin and
    /// target with the elements of its one deleted path.
    fn deleted_path(bytes: &[u8]) -> PathKey {
        let n = proto::Notification::decode(bytes).expect("a notification");
        assert!(n.update.is_empty(), "a delete carries no update");
        assert_eq!(n.delete.len(), 1, "one deleted path");
        let prefix = n.prefix.unwrap_or_default();
        PathKey::new(proto::Path {
            origin: prefix.origin,
            elem: n.delete[0].elem.clone(),
            target: prefix.target,
        })
    }

    /// Releasing a series' last publisher broadcasts exactly one removal,
    /// whose notification deletes the series' path; the series is gone.
    #[tokio::test]
    async fn releasing_the_last_publisher_broadcasts_one_removal() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, &note(1.0))
            .expect("one owner");
        let mut receiver = target.subscribe();
        target.release(OWNER, [id]);
        match receiver.try_recv().expect("a removal") {
            SeriesEvent::Removed { series_id, bytes } => {
                assert_eq!(series_id, id);
                assert_eq!(deleted_path(&bytes), series);
            }
            other => panic!("a removal, got {other:?}"),
        }
        assert_eq!(
            receiver.try_recv().map(|_| ()),
            Err(broadcast::error::TryRecvError::Empty),
            "one removal"
        );
        assert_eq!(target.series_count(), 0);
    }

    /// Releasing a publisher of a series another publisher still holds
    /// broadcasts nothing, and the series stays.
    #[tokio::test]
    async fn releasing_a_publisher_that_is_not_the_last_broadcasts_nothing() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, &note(1.0))
            .expect("one owner");
        target
            .publish(OWNER, None, &series, &note(1.0))
            .expect("one owner");
        assert_eq!(
            target.publishers(id),
            Some(2),
            "the premise: two publishers"
        );
        let mut receiver = target.subscribe();
        target.release(OWNER, [id]);
        assert_eq!(
            receiver.try_recv().map(|_| ()),
            Err(broadcast::error::TryRecvError::Empty)
        );
        assert_eq!(target.series_count(), 1);
    }

    /// A series released while another thread keeps publishing it: the
    /// receiver's last event for the released id is its removal, every
    /// round. The publisher keeps the id each publish returns, so once the
    /// series has left it publishes a new series on the same path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_value_is_received_after_a_series_removal() {
        use std::sync::atomic::{AtomicBool, AtomicUsize};
        const ROUNDS: usize = 200;
        let target = bind(1 << 16).await;
        let mut receiver = target.subscribe();
        let mut crossed = 0;
        for round in 0..ROUNDS {
            let series = path(&format!("Gi{round}"), "in-octets");
            let (id, _) = target
                .publish(OWNER, None, &series, &note(0.0))
                .expect("one owner");
            let published = AtomicUsize::new(0);
            let stop = AtomicBool::new(false);
            let last_id = std::thread::scope(|scope| {
                let publisher = scope.spawn(|| {
                    let mut current = id;
                    let mut value = 0.0;
                    while !stop.load(Ordering::Relaxed) {
                        value += 1.0;
                        current = target
                            .publish(OWNER, Some(current), &series, &note(value))
                            .expect("one owner")
                            .0;
                        published.fetch_add(1, Ordering::Relaxed);
                    }
                    current
                });
                while published.load(Ordering::Relaxed) < 10 {
                    std::hint::spin_loop();
                }
                target.release(OWNER, [id]);
                let after = published.load(Ordering::Relaxed);
                while published.load(Ordering::Relaxed) < after + 10 {
                    std::hint::spin_loop();
                }
                stop.store(true, Ordering::Relaxed);
                publisher.join().expect("the publisher ran")
            });
            if last_id != id {
                crossed += 1;
            }
            target.release(OWNER, [last_id]);
            let mut last_for_id = None;
            while let Ok(event) = receiver.try_recv() {
                match event {
                    SeriesEvent::Changed(update) if update.series_id == id => {
                        last_for_id = Some("a value");
                    }
                    SeriesEvent::Removed { series_id, .. } if series_id == id => {
                        assert_ne!(
                            last_for_id,
                            Some("a removal"),
                            "round {round}: two removals"
                        );
                        last_for_id = Some("a removal");
                    }
                    _ => {}
                }
            }
            assert_eq!(last_for_id, Some("a removal"), "round {round}");
        }
        assert_eq!(
            crossed, ROUNDS,
            "the premise: publishes kept coming after the removal"
        );
    }

    /// A series' version is 1 after its first value and rises by one with
    /// each change, through a publish or an overwrite, with or without a
    /// receiver; an unchanged value keeps it. A broadcast update carries the
    /// version the snapshot then shows.
    #[tokio::test]
    async fn the_version_rises_by_one_with_each_change() {
        let target = bind(16).await;
        let mut receiver = target.subscribe();
        let series = path("Gi0", "in-octets");
        let version = || target.snapshot()[0].version;

        let (id, _) = target
            .publish(OWNER, None, &series, &note_at(1.0, 1_000))
            .expect("one owner");
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).version,
            1
        );
        assert_eq!(version(), 1, "the first value");

        target
            .publish(OWNER, Some(id), &series, &note_at(1.0, 2_000))
            .expect("one owner");
        assert_eq!(version(), 1, "an unchanged value keeps the version");

        target
            .publish(OWNER, Some(id), &series, &note_at(2.0, 3_000))
            .expect("one owner");
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).version,
            2
        );
        assert_eq!(version(), 2, "a published change");

        target
            .overwrite(&series, &note_at(3.0, 4_000))
            .expect("a notification")
            .expect("a served path");
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).version,
            3
        );
        assert_eq!(version(), 3, "an overwritten change");

        drop(receiver);
        target
            .publish(OWNER, Some(id), &series, &note_at(4.0, 5_000))
            .expect("one owner");
        assert_eq!(version(), 4, "a change with no receiver");
    }

    /// The value a series is compared by lies inside its stored
    /// notification: located, not copied.
    #[tokio::test]
    async fn the_value_is_a_slice_of_the_latest_notification() {
        let target = bind(16).await;
        let buf = note(7.0);
        let (id, _) = target
            .publish(OWNER, None, &path("Gi0", "in-octets"), &buf)
            .expect("one owner");
        let table = target.read();
        let latest = lock(&table.slots[&id].latest);
        let (bytes, value) = (latest.bytes.as_ptr_range(), latest.value().as_ptr_range());
        assert!(
            bytes.start <= value.start && value.end <= bytes.end && value.start < value.end,
            "the value {value:?} is not inside the notification {bytes:?}"
        );
        let decoded = proto::TypedValue::decode(latest.value()).expect("a typed value");
        assert_eq!(
            decoded.value,
            Some(proto::typed_value::Value::DoubleVal(7.0))
        );
    }

    /// A series' kind is its first publish's; a later value of another kind
    /// is stored without changing it.
    #[tokio::test]
    async fn the_kind_is_fixed_at_the_first_publish() {
        let target = bind(16).await;
        let mut receiver = target.subscribe();
        let event = MetricEvent::new(
            "in_octets".to_string(),
            1.0,
            Labels::from_pairs(&[("ifName", "Gi0")]).expect("valid labels"),
        )
        .expect("valid event");
        let enumeration = GnmiValueType::Enum(BTreeMap::from([(1, "UP".to_string())]));
        let (as_string, as_uint) = (
            encode(&event, enumeration),
            encode(&event, GnmiValueType::Uint),
        );
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, &as_string)
            .expect("one owner");
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).kind,
            ValueKind::String
        );
        let (_, done) = target
            .publish(OWNER, Some(id), &series, &as_uint)
            .expect("one owner");
        assert!(done.changed, "another kind of value is another value");
        assert_eq!(
            as_change(receiver.try_recv().expect("broadcast")).kind,
            ValueKind::String,
            "broadcast with the first kind"
        );
        let snapshot = target.snapshot();
        assert_eq!(snapshot[0].kind, ValueKind::String);
        assert_eq!(snapshot[0].latest, as_uint.as_slice());

        let (_, _) = target
            .publish(OWNER, None, &path("Gi1", "in-octets"), &as_uint)
            .expect("one owner");
        let kinds: Vec<ValueKind> = {
            let mut snapshot = target.snapshot();
            snapshot.sort_by_key(|s| s.series_id);
            snapshot.into_iter().map(|s| s.kind).collect()
        };
        assert_eq!(kinds, [ValueKind::String, ValueKind::Uint]);
    }

    /// Bytes without a locatable value are refused on every publish, a
    /// series' first or a later one, and by an overwrite, and leave the
    /// stored value as it was.
    #[tokio::test]
    async fn a_notification_without_a_value_is_refused_on_every_publish() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let no_value = proto::Notification {
            timestamp: 1,
            update: vec![proto::Update {
                path: Some(series.path().clone()),
                val: None,
            }],
            ..Default::default()
        }
        .encode_to_vec();
        let refused = |err: SondaError| {
            assert!(matches!(err, SondaError::Sink(_)), "got {err:?}");
            assert!(
                err.to_string().contains("not a gnmi notification"),
                "got {err}"
            );
        };
        refused(
            target
                .publish(OWNER, None, &series, &no_value)
                .expect_err("a first publish without a value"),
        );
        assert_eq!(target.series_count(), 0, "nothing was stored");

        let value = note(1.0);
        let (id, _) = target
            .publish(OWNER, None, &series, &value)
            .expect("one owner");
        for bytes in [no_value.as_slice(), b"bytes"] {
            refused(
                target
                    .publish(OWNER, Some(id), &series, bytes)
                    .expect_err("a later publish without a value"),
            );
            refused(
                target
                    .overwrite(&series, bytes)
                    .expect_err("an overwrite without a value"),
            );
        }
        assert_eq!(target.snapshot()[0].latest, value, "the value is kept");
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
        let (a, b, c) = (note(1.0), note(2.0), note(3.0));
        let (first, _) = target
            .publish(OWNER, None, &path("Gi0", "in-octets"), &a)
            .expect("one owner");
        assert_eq!(first, 0);
        target.write().next_id = SeriesId::MAX;
        let (last, _) = target
            .publish(OWNER, None, &path("Gi1", "in-octets"), &b)
            .expect("one owner");
        assert_eq!(last, SeriesId::MAX);
        let (wrapped, _) = target
            .publish(OWNER, None, &path("Gi2", "in-octets"), &c)
            .expect("one owner");
        assert_ne!(wrapped, first, "id 0 is still in use");
        assert_eq!(wrapped, 1);
        assert_eq!(target.series_count(), 3);
        let latest: Vec<Bytes> = {
            let mut snapshot = target.snapshot();
            snapshot.sort_by_key(|s| s.series_id);
            snapshot.into_iter().map(|s| s.latest).collect()
        };
        assert_eq!(latest, [a, c, b]);
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
            .publish(OWNER, None, &served, &note(1.0))
            .expect("one owner");

        let posted = note(2.0);
        assert_eq!(
            target.overwrite(&served, &posted).expect("a value"),
            Some(id)
        );
        assert_eq!(target.snapshot()[0].latest, posted);
        assert_eq!(
            target
                .overwrite(&path("Gi9", "in-octets"), &posted)
                .expect("a value"),
            None
        );
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
            .publish(OWNER, None, &series, &note(-1.0))
            .expect("one owner");
        // Every value is distinct, so every publish is a change.
        let values: Vec<Bytes> = (0..ROUNDS * WRITERS).map(|i| note(i as f64)).collect();
        let barrier = Barrier::new(WRITERS);
        let (checked, mismatches) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let (target, series, barrier, values) = (&target, &series, &barrier, &values);
                let (checked, mismatches) = (&checked, &mismatches);
                scope.spawn(move || {
                    let mut receiver = (writer == 0).then(|| target.subscribe());
                    for round in 0..ROUNDS {
                        barrier.wait();
                        target
                            .publish(OWNER, Some(id), series, &values[round * WRITERS + writer])
                            .expect("one owner");
                        barrier.wait();
                        if let Some(receiver) = receiver.as_mut() {
                            let mut last = None;
                            while let Ok(event) = receiver.try_recv() {
                                last = Some(as_change(event).bytes);
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
            .publish(OWNER, None, &series, &note(-1.0))
            .expect("one owner");
        // Every value is distinct, so every publish is a change.
        let values: Vec<Bytes> = (0..ROUNDS).map(|i| note(i as f64)).collect();
        let barrier = Barrier::new(2);
        let missed = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let (target, series, barrier, values) = (&target, &series, &barrier, &values);
            scope.spawn(move || {
                for value in values {
                    barrier.wait();
                    target
                        .publish(OWNER, Some(id), series, value)
                        .expect("one owner");
                    barrier.wait();
                    barrier.wait();
                }
            });
            for published in values {
                barrier.wait();
                let mut receiver = target.subscribe();
                let mut seen = target.snapshot()[0].latest.clone();
                barrier.wait();
                while let Ok(event) = receiver.try_recv() {
                    seen = as_change(event).bytes;
                }
                if seen != *published {
                    missed.fetch_add(1, Ordering::Relaxed);
                }
                drop(receiver);
                barrier.wait();
            }
        });
        assert_eq!(target.tx.receiver_count(), 0, "every round's receiver left");
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

    /// A target serving TLS from `tls`, on an ephemeral port.
    async fn bind_tls(tls: crate::sink::gnmi::GnmiTlsConfig) -> Arc<GnmiTarget> {
        let cfg = GnmiTargetConfig {
            tls: Some(tls),
            ..GnmiTargetConfig::new("127.0.0.1:0")
        };
        GnmiTarget::bind(cfg.settings().expect("valid settings"))
            .await
            .expect("bind an ephemeral port")
    }

    /// Connect to `addr` and subscribe ONCE; the error of whichever step
    /// failed, as text. Fails the test when neither finishes within 5 s.
    async fn attempt(
        connect: impl Future<Output = Result<tonic::transport::Channel, tonic::transport::Error>>,
    ) -> Result<bool, String> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let channel = connect.await.map_err(|e| error_chain(&e))?;
            crate::sink::gnmi::test_tls::once_syncs(channel)
                .await
                .map_err(|s| s.to_string())
        })
        .await
        .expect("the attempt ends with an answer, not a timeout")
    }

    #[tokio::test]
    async fn a_tls_client_trusting_the_ca_receives_the_sync() {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let target = bind_tls(files.tls()).await;
        let synced = attempt(files.channel(target.local_addr(), false)).await;
        assert_eq!(synced, Ok(true));
    }

    #[tokio::test]
    async fn a_plaintext_client_is_refused_by_a_tls_target() {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let target = bind_tls(files.tls()).await;
        let addr = target.local_addr();
        assert_eq!(
            attempt(files.channel(addr, false)).await,
            Ok(true),
            "the premise: the target serves a TLS client"
        );

        let plaintext =
            tonic::transport::Endpoint::from_shared(format!("http://{addr}")).expect("a valid uri");
        let refused = attempt(plaintext.connect()).await;
        assert!(
            refused.is_err(),
            "a plaintext client is served: {refused:?}"
        );
    }

    #[tokio::test]
    async fn with_client_ca_only_a_client_with_a_signed_certificate_is_served() {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let target = bind_tls(files.mutual_tls()).await;
        let addr = target.local_addr();

        let refused = attempt(files.channel(addr, false)).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("CertificateRequired")),
            "a client without a certificate is refused at the handshake: {refused:?}"
        );
        assert_eq!(attempt(files.channel(addr, true)).await, Ok(true));
    }

    #[tokio::test]
    async fn without_client_ca_a_client_without_a_certificate_is_served() {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let target = bind_tls(files.tls()).await;
        assert_eq!(
            attempt(files.channel(target.local_addr(), false)).await,
            Ok(true)
        );
    }

    /// A client that opens a TCP connection to a TLS target and sends
    /// nothing is closed once the handshake timeout has passed.
    #[tokio::test]
    async fn a_connection_that_never_starts_its_handshake_is_closed_at_the_timeout() {
        use tokio::io::AsyncReadExt;
        const TIMEOUT: Duration = Duration::from_millis(200);
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let mut settings = GnmiTargetConfig {
            tls: Some(files.tls()),
            ..GnmiTargetConfig::new("127.0.0.1:0")
        }
        .settings()
        .expect("valid settings");
        settings.tls_handshake_timeout = TIMEOUT;
        let target = GnmiTarget::bind(settings)
            .await
            .expect("bind an ephemeral port");

        let mut socket = TcpStream::connect(target.local_addr())
            .await
            .expect("connect");
        let connected = tokio::time::Instant::now();
        let mut byte = [0u8; 1];
        let early = tokio::time::timeout(TIMEOUT / 2, socket.read(&mut byte)).await;
        assert!(
            early.is_err(),
            "the premise: the target holds the connection while the timeout runs: {early:?}"
        );
        let closed = tokio::time::timeout_at(connected + 2 * TIMEOUT, socket.read(&mut byte)).await;
        assert!(
            matches!(closed, Ok(Ok(0) | Err(_))),
            "the connection is open {:?} after it was made: {closed:?}",
            connected.elapsed()
        );
    }

    /// TLS files that make no server configuration fail the bind with an
    /// error naming the listen address.
    #[tokio::test]
    async fn a_tls_error_at_bind_names_the_address() {
        let files = crate::sink::gnmi::test_tls::TlsFiles::new();
        let mut settings = GnmiTargetConfig::new("127.0.0.1:0")
            .settings()
            .expect("valid settings");
        settings.tls = Some(crate::sink::gnmi::TlsIdentity {
            paths: files.tls(),
            cert: b"not a certificate".to_vec(),
            key: b"not a key".to_vec(),
            client_ca: None,
        });
        let error = match GnmiTarget::bind(settings).await {
            Ok(_) => panic!("TLS without a certificate binds"),
            Err(error) => error,
        };
        assert!(matches!(error, SondaError::Sink(_)), "got {error:?}");
        assert!(
            error
                .to_string()
                .contains("cannot serve TLS on 127.0.0.1:0"),
            "got {error}"
        );
    }

    /// The `json_ietf_val` text of `value`, one encoded `TypedValue`.
    fn json_ietf_text(value: &[u8]) -> String {
        match proto::TypedValue::decode(value)
            .expect("a typed value")
            .value
        {
            Some(proto::typed_value::Value::JsonIetfVal(text)) => {
                String::from_utf8(text).expect("UTF-8 text")
            }
            other => panic!("not a json_ietf_val: {other:?}"),
        }
    }

    /// How many JSON_IETF values `target` has made.
    fn json_ietf_values(target: &GnmiTarget) -> usize {
        target
            .settings
            .probe
            .json_ietf_values
            .load(Ordering::Relaxed)
    }

    /// The only series' version and latest notification.
    fn only_series(target: &GnmiTarget) -> (SeriesId, u64, Bytes) {
        let mut snapshot = target.snapshot();
        assert_eq!(snapshot.len(), 1, "the premise: one series");
        let series = snapshot.remove(0);
        (series.series_id, series.version, series.latest)
    }

    /// Sixteen callers asking at once for a series' JSON_IETF value make it
    /// once and all hold the same buffer. A publish that does not change the
    /// value keeps it; after a change the next caller makes the new one.
    #[tokio::test]
    async fn a_json_ietf_value_is_made_once_per_change() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, &note_at(1.5, 1_000))
            .expect("one owner");
        let (_, version, latest) = only_series(&target);

        let barrier = std::sync::Barrier::new(16);
        let values: Vec<Bytes> = std::thread::scope(|scope| {
            let callers: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        target.json_ietf_value(id, version, &latest)
                    })
                })
                .collect();
            callers
                .into_iter()
                .map(|caller| caller.join().expect("caller").expect("a value"))
                .collect()
        });
        assert_eq!(values.len(), 16);
        assert_eq!(
            json_ietf_values(&target),
            1,
            "made once for sixteen callers"
        );
        assert!(
            values.iter().all(|v| v.as_ptr() == values[0].as_ptr()),
            "every caller holds the same buffer"
        );
        assert_eq!(json_ietf_text(&values[0]), "1.5");

        target
            .publish(OWNER, Some(id), &series, &note_at(1.5, 2_000))
            .expect("one owner");
        let (_, unchanged, republished) = only_series(&target);
        assert_eq!(unchanged, version, "the premise: the value did not change");
        assert_ne!(republished, latest, "the premise: the notification did");
        let kept = target
            .json_ietf_value(id, unchanged, &republished)
            .expect("a value");
        assert_eq!(
            kept.as_ptr(),
            values[0].as_ptr(),
            "kept through the publish"
        );
        assert_eq!(json_ietf_values(&target), 1);

        target
            .publish(OWNER, Some(id), &series, &note_at(2.5, 3_000))
            .expect("one owner");
        let (_, changed, newest) = only_series(&target);
        assert_eq!(changed, version + 1, "the premise: a change");
        let remade = target
            .json_ietf_value(id, changed, &newest)
            .expect("a value");
        assert_eq!(json_ietf_values(&target), 2, "made again after the change");
        assert_ne!(remade.as_ptr(), values[0].as_ptr());
        assert_eq!(json_ietf_text(&remade), "2.5");
    }

    /// A caller holding a version the series has moved past gets that
    /// version's value, made from the notification it holds and not kept.
    /// An unknown id has none, and nothing is made for it.
    #[tokio::test]
    async fn a_json_ietf_value_of_a_past_version_is_made_from_the_callers_notification() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let first = note_at(1.5, 1_000);
        let (id, _) = target
            .publish(OWNER, None, &series, &first)
            .expect("one owner");
        target
            .publish(OWNER, Some(id), &series, &note_at(2.5, 2_000))
            .expect("one owner");
        let (_, version, latest) = only_series(&target);
        assert_eq!(version, 2, "the premise: the series moved past version 1");

        let past = target.json_ietf_value(id, 1, &first).expect("a value");
        assert_eq!(json_ietf_text(&past), "1.5", "the value the caller holds");
        let current = target
            .json_ietf_value(id, version, &latest)
            .expect("a value");
        assert_eq!(
            json_ietf_text(&current),
            "2.5",
            "the past value was not kept"
        );
        assert_eq!(json_ietf_values(&target), 2);

        assert_eq!(target.json_ietf_value(id.wrapping_add(1), 1, &first), None);
        assert_eq!(target.json_ietf_of(id.wrapping_add(1), 1, &first), None);
        assert_eq!(
            json_ietf_values(&target),
            2,
            "nothing made for an unknown id"
        );
    }

    /// `json_ietf_of` is the caller's notification with the JSON_IETF value
    /// in it: through a publish that does not change the value, the value is
    /// made once and each notification keeps its own timestamp.
    #[tokio::test]
    async fn json_ietf_of_carries_the_callers_timestamp() {
        let target = bind(16).await;
        let series = path("Gi0", "in-octets");
        let (id, _) = target
            .publish(OWNER, None, &series, &note_at(1.5, 1_000))
            .expect("one owner");
        let (_, version, first) = only_series(&target);
        let at_first = target
            .json_ietf_of(id, version, &first)
            .expect("a notification");
        target
            .publish(OWNER, Some(id), &series, &note_at(1.5, 2_000))
            .expect("one owner");
        let (_, unchanged, second) = only_series(&target);
        assert_eq!(unchanged, version, "the premise: the value did not change");
        let at_second = target
            .json_ietf_of(id, unchanged, &second)
            .expect("a notification");
        assert_eq!(json_ietf_values(&target), 1, "the value was made once");

        let json = Some(proto::TypedValue {
            value: Some(proto::typed_value::Value::JsonIetfVal(b"1.5".to_vec())),
        });
        for (bytes, timestamp) in [(&at_first, 1_000), (&at_second, 2_000)] {
            let notification = proto::Notification::decode(bytes.as_ref()).expect("decodes");
            assert_eq!(notification.timestamp, timestamp);
            assert_eq!(notification.update[0].val, json);
        }
    }
}
