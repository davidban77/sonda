//! A process-wide map from listen address to the live listener target bound
//! there, generic over the target type.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use crate::{ConfigError, SondaError};

/// Longest an acquire waits for a stopped target's listener on the same port
/// to close before binding anyway.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// A target a [`ListenerRegistry`] shares between the sinks that name its
/// address.
pub(crate) trait ListenerTarget: Send + Sync + 'static {
    /// The resolved settings a target is bound with and compared against.
    type Config;

    /// What the registry keeps of a target's listening socket. It outlives
    /// the target, so the registry can wait for the socket of a target whose
    /// last handle dropped to close.
    type SocketState: Clone + Send + Sync + 'static;

    /// The sink type named in the registry's errors, e.g. `gnmi_target`.
    const SINK_TYPE: &'static str;

    /// Bind `cfg`'s listen address and start serving.
    fn bind(cfg: Self::Config) -> impl Future<Output = Result<Arc<Self>, SondaError>> + Send;

    /// The address the listener bound, with an ephemeral port resolved.
    fn local_addr(&self) -> SocketAddr;

    /// Whether the listener task is still running.
    fn is_live(&self) -> bool;

    /// `Ok` when a target running with its settings also serves `cfg`;
    /// otherwise the [`SondaError::Config`] saying how they differ.
    fn serves(&self, cfg: &Self::Config) -> Result<(), SondaError>;

    /// The target's socket state, for the registry to keep.
    fn socket_state(&self) -> Self::SocketState;

    /// Whether the listening socket `state` describes may still be open.
    fn socket_open(state: &Self::SocketState) -> bool;

    /// Resolves when the listening socket `state` describes is closed.
    fn closed(state: Self::SocketState) -> impl Future<Output = ()> + Send;
}

/// One registered target: a weak handle, and its socket state, which
/// outlives the target until the listener has closed.
struct Entry<T: ListenerTarget> {
    target: Weak<T>,
    socket: T::SocketState,
}

impl<T: ListenerTarget> Entry<T> {
    /// The target, while a handle to it exists and its listener task runs.
    fn live(&self) -> Option<Arc<T>> {
        self.target.upgrade().filter(|target| target.is_live())
    }

    /// Whether the target is not live but its listening socket may still be
    /// open.
    fn releasing(&self) -> bool {
        self.live().is_none() && T::socket_open(&self.socket)
    }
}

/// Shares one target between every acquire that names its address.
///
/// The map holds weak handles, so a target lives exactly as long as some
/// sink holds it. Dropping the last handle stops the target and its
/// listening socket closes; an acquire of the same port waits for that
/// before binding, and the stale entry is pruned once it has.
pub(crate) struct ListenerRegistry<T: ListenerTarget> {
    inner: Mutex<HashMap<SocketAddr, Entry<T>>>,
    /// Held across a bind so concurrent acquires of one address bind once.
    /// The map lock above is never held across an `.await`.
    binding: tokio::sync::Mutex<()>,
}

impl<T: ListenerTarget> ListenerRegistry<T> {
    /// An empty registry.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            binding: tokio::sync::Mutex::new(()),
        }
    }

    /// The live target for `listen`, binding one with `cfg` if there is
    /// none. A listen address with port 0 always binds a new target. A
    /// target whose listener task has exited is not live, even while handles
    /// to it remain. Before binding, waits up to two seconds in all for the
    /// listening socket of any target that is no longer live, on an address
    /// that overlaps this one, to close.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] when a live target on the address serves
    /// different settings, or when a live target on another address contends
    /// for the same port (a wildcard address and an address it covers); the
    /// error of [`ListenerTarget::bind`] when binding fails.
    pub(crate) async fn acquire(
        &self,
        cfg: T::Config,
        listen: SocketAddr,
    ) -> Result<Arc<T>, SondaError> {
        let deadline = tokio::time::Instant::now() + RELEASE_TIMEOUT;
        let _binding = loop {
            if listen.port() != 0 {
                // Outside the bind lock, so other acquires do not queue
                // behind this wait.
                self.released(listen, deadline).await;
            }
            let binding = self.binding.lock().await;
            if listen.port() == 0 {
                break binding;
            }
            if let Some(target) = self.live(listen) {
                return check_settings(target, &cfg);
            }
            self.check_contending(listen)?;
            // A target may have stopped while this acquire waited for the
            // lock; wait for it again, after releasing the lock.
            if self.releasing(listen) && tokio::time::Instant::now() < deadline {
                continue;
            }
            break binding;
        };
        let target = T::bind(cfg).await?;
        let mut map = self.map();
        map.retain(|_, entry| entry.live().is_some() || entry.releasing());
        map.insert(
            target.local_addr(),
            Entry {
                target: Arc::downgrade(&target),
                socket: target.socket_state(),
            },
        );
        Ok(target)
    }

    /// Whether [`acquire`](Self::acquire) of `cfg` on `listen` would be
    /// refused by the targets running now, without binding anything: a live
    /// target on the address runs with different settings, or a live target
    /// on another address contends for the port.
    ///
    /// # Errors
    ///
    /// The [`SondaError::Config`] that `acquire` would return for those
    /// cases.
    pub(crate) fn check(&self, cfg: &T::Config, listen: SocketAddr) -> Result<(), SondaError> {
        if listen.port() == 0 {
            return Ok(());
        }
        if let Some(target) = self.live(listen) {
            return check_settings(target, cfg).map(|_| ());
        }
        self.check_contending(listen)
    }

    /// The live target that serves `listen`, without ever binding: the
    /// target on that address, or a wildcard target on the same port that
    /// covers it. `wanted` gives the settings to compare with the target's,
    /// from the target found.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] when no live target serves the address, or
    /// when the target does not serve what `wanted` returns.
    pub(crate) fn join(
        &self,
        listen: SocketAddr,
        wanted: impl FnOnce(&T) -> T::Config,
    ) -> Result<Arc<T>, SondaError> {
        let target = (listen.port() != 0)
            .then(|| self.serving(listen))
            .flatten()
            .ok_or_else(|| {
                SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario listens on {listen}"
                )))
            })?;
        let wanted = wanted(&target);
        check_settings(target, &wanted)
    }

    /// Wait, until `deadline` at the latest, for every target that is no
    /// longer live but may still listen on an address overlapping `listen`
    /// to close its socket.
    async fn released(&self, listen: SocketAddr, deadline: tokio::time::Instant) {
        let releasing: Vec<T::SocketState> = self
            .map()
            .iter()
            .filter(|(addr, entry)| listen_overlap(**addr, listen) && entry.releasing())
            .map(|(_, entry)| entry.socket.clone())
            .collect();
        for socket in releasing {
            let _ = tokio::time::timeout_at(deadline, T::closed(socket)).await;
        }
    }

    /// Whether a target that is no longer live may still listen on an
    /// address overlapping `listen`.
    fn releasing(&self, listen: SocketAddr) -> bool {
        self.map()
            .iter()
            .any(|(addr, entry)| listen_overlap(*addr, listen) && entry.releasing())
    }

    /// Live targets.
    pub(crate) fn len(&self) -> usize {
        self.map()
            .values()
            .filter(|entry| entry.live().is_some())
            .count()
    }

    /// Whether no target is live.
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The live targets, sorted by the address each listens on.
    pub(crate) fn targets(&self) -> Vec<Arc<T>> {
        let mut targets: Vec<Arc<T>> = self.map().values().filter_map(Entry::live).collect();
        targets.sort_by_key(|target| target.local_addr());
        targets
    }

    fn live(&self, addr: SocketAddr) -> Option<Arc<T>> {
        self.map().get(&addr).and_then(Entry::live)
    }

    /// The live target on `addr`, or else a live wildcard target on the same
    /// port that covers `addr`.
    fn serving(&self, addr: SocketAddr) -> Option<Arc<T>> {
        self.live(addr).or_else(|| {
            self.map()
                .iter()
                .filter(|(wild, _)| covers(**wild, addr))
                .find_map(|(_, entry)| entry.live())
        })
    }

    /// [`SondaError::Config`] when a live target on another address contends
    /// with `listen` for its port.
    fn check_contending(&self, listen: SocketAddr) -> Result<(), SondaError> {
        match self.contending(listen) {
            Some(running) => Err(SondaError::Config(ConfigError::invalid(format!(
                "{} {listen} cannot listen while the running target on {running} \
                 serves port {}; use {running} for both",
                T::SINK_TYPE,
                listen.port()
            )))),
            None => Ok(()),
        }
    }

    /// The address of a live target on another address that contends with
    /// `addr` for its port.
    fn contending(&self, addr: SocketAddr) -> Option<SocketAddr> {
        self.map()
            .iter()
            .filter(|(other, entry)| {
                **other != addr && listen_overlap(**other, addr) && entry.live().is_some()
            })
            .map(|(other, _)| *other)
            .next()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<SocketAddr, Entry<T>>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn check_settings<T: ListenerTarget>(
    target: Arc<T>,
    wanted: &T::Config,
) -> Result<Arc<T>, SondaError> {
    target.serves(wanted).map(|()| target)
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
    let (a, b) = (canonical(a), canonical(b));
    if a.port() == 0 || a.port() != b.port() {
        return false;
    }
    a == b || covers(a, b) || covers(b, a)
}

/// `addr` with an IPv4-mapped IPv6 address (`[::ffff:127.0.0.1]`) read as
/// the IPv4 address it maps, which is the socket it binds.
pub(crate) fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Whether `wild` is a wildcard listening on `other`'s port that accepts
/// `other`'s family, one way: `0.0.0.0` covers IPv4 addresses, `[::]` covers
/// both families (see [`listen_overlap`] for the dual-stack assumption).
pub(crate) fn covers(wild: SocketAddr, other: SocketAddr) -> bool {
    let (wild, other) = (canonical(wild), canonical(other));
    wild.port() != 0
        && wild.port() == other.port()
        && wild.ip().is_unspecified()
        && (wild.is_ipv6() || other.is_ipv4())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

    use tokio::sync::watch;

    use super::*;

    /// Fake ports handed out for port 0, counting down so that targets
    /// acquired one after another are listed in the reverse of that order.
    static NEXT_PORT: AtomicU16 = AtomicU16::new(60999);

    /// A target with no socket: an address, a liveness flag, and a socket
    /// state the test opens and closes by hand.
    struct Fake {
        addr: SocketAddr,
        tag: u32,
        live: AtomicBool,
        socket: Arc<watch::Sender<bool>>,
        /// Whether dropping the target closes its socket, as a real target's
        /// last drop does.
        closes_on_drop: bool,
    }

    struct FakeConfig {
        listen: SocketAddr,
        /// The one setting compared between two acquires of an address.
        tag: u32,
        closes_on_drop: bool,
    }

    impl FakeConfig {
        fn new(listen: &str, tag: u32) -> Self {
            Self {
                listen: listen.parse().expect("a socket address"),
                tag,
                closes_on_drop: true,
            }
        }
    }

    impl ListenerTarget for Fake {
        type Config = FakeConfig;
        type SocketState = watch::Receiver<bool>;
        const SINK_TYPE: &'static str = "fake_target";

        async fn bind(cfg: FakeConfig) -> Result<Arc<Self>, SondaError> {
            let mut addr = cfg.listen;
            if addr.port() == 0 {
                addr.set_port(NEXT_PORT.fetch_sub(1, Ordering::Relaxed));
            }
            Ok(Arc::new(Fake {
                addr,
                tag: cfg.tag,
                live: AtomicBool::new(true),
                socket: Arc::new(watch::channel(true).0),
                closes_on_drop: cfg.closes_on_drop,
            }))
        }

        fn local_addr(&self) -> SocketAddr {
            self.addr
        }

        fn is_live(&self) -> bool {
            self.live.load(Ordering::Relaxed)
        }

        fn serves(&self, cfg: &FakeConfig) -> Result<(), SondaError> {
            if self.tag == cfg.tag {
                return Ok(());
            }
            Err(SondaError::Config(ConfigError::invalid(format!(
                "tag {} and {}",
                self.tag, cfg.tag
            ))))
        }

        fn socket_state(&self) -> watch::Receiver<bool> {
            self.socket.subscribe()
        }

        fn socket_open(state: &watch::Receiver<bool>) -> bool {
            *state.borrow()
        }

        async fn closed(mut state: watch::Receiver<bool>) {
            let _ = state.wait_for(|open| !open).await;
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            if self.closes_on_drop {
                self.socket.send_replace(false);
            }
        }
    }

    async fn acquire(
        registry: &ListenerRegistry<Fake>,
        cfg: FakeConfig,
    ) -> Result<Arc<Fake>, SondaError> {
        let listen = cfg.listen;
        registry.acquire(cfg, listen).await
    }

    /// A live wildcard target refuses an acquire of an address it covers on
    /// its port, naming itself, and `check` refuses it with the same error.
    #[tokio::test]
    async fn an_overlapping_address_is_refused() {
        let registry = ListenerRegistry::<Fake>::new();
        let wild = acquire(&registry, FakeConfig::new("0.0.0.0:0", 1))
            .await
            .expect("the first acquire binds");
        let port = wild.local_addr().port();
        assert_ne!(port, 0, "the bound port is read back");
        assert_eq!(registry.len(), 1, "the first target is live");

        let specific = format!("127.0.0.1:{port}");
        let err = acquire(&registry, FakeConfig::new(&specific, 1))
            .await
            .err()
            .expect("a second listener on the port is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("fake_target {specific} cannot listen"))
                && msg.contains(&format!("0.0.0.0:{port}")),
            "got {msg}"
        );
        let checked = registry
            .check(&FakeConfig::new(&specific, 1), specific.parse().unwrap())
            .expect_err("check refuses what acquire refuses");
        assert_eq!(checked.to_string(), msg);
        assert_eq!(registry.len(), 1, "nothing else was bound");
    }

    /// Port 0 binds a new target on every acquire, whatever its settings,
    /// while a bound address shares its target.
    #[tokio::test]
    async fn port_zero_binds_fresh() {
        let registry = ListenerRegistry::<Fake>::new();
        let first = acquire(&registry, FakeConfig::new("127.0.0.1:0", 1))
            .await
            .expect("bind");
        let second = acquire(&registry, FakeConfig::new("127.0.0.1:0", 2))
            .await
            .expect("port 0 never compares settings");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_ne!(first.local_addr(), second.local_addr());
        assert_eq!(registry.len(), 2, "both are live");

        let shared = acquire(
            &registry,
            FakeConfig::new(&first.local_addr().to_string(), 1),
        )
        .await
        .expect("a bound address is joined");
        assert!(Arc::ptr_eq(&first, &shared));
        assert_eq!(registry.len(), 2);
    }

    /// A target whose listener stopped is not joined though a handle to it
    /// remains, and the next acquire of its address binds a new one.
    #[tokio::test]
    async fn a_dead_target_is_not_joined() {
        let registry = ListenerRegistry::<Fake>::new();
        let old = acquire(&registry, FakeConfig::new("127.0.0.1:0", 1))
            .await
            .expect("bind");
        let addr = old.local_addr();
        let joined = registry
            .join(addr, |_| FakeConfig::new(&addr.to_string(), 1))
            .expect("a live target is joined");
        assert!(Arc::ptr_eq(&old, &joined));

        old.live.store(false, Ordering::Relaxed);
        old.socket.send_replace(false);
        let err = registry
            .join(addr, |_| FakeConfig::new(&addr.to_string(), 1))
            .err()
            .expect("a dead target is not joined");
        assert!(
            err.to_string().contains("no running scenario listens on"),
            "got {err}"
        );
        assert!(registry.is_empty(), "a dead target is not live");

        let fresh = acquire(&registry, FakeConfig::new(&addr.to_string(), 1))
            .await
            .expect("a fresh target binds the address");
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert_eq!(fresh.local_addr(), addr);
    }

    /// `targets()` lists the live targets in address order and leaves out
    /// one whose listener stopped and one whose last handle dropped.
    #[tokio::test]
    async fn targets_are_sorted_and_only_live() {
        let registry = ListenerRegistry::<Fake>::new();
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(
                acquire(&registry, FakeConfig::new("127.0.0.1:0", 1))
                    .await
                    .expect("bind"),
            );
        }
        let acquired: Vec<SocketAddr> = held.iter().map(|t| t.local_addr()).collect();
        let mut sorted = acquired.clone();
        sorted.sort();
        assert_ne!(acquired, sorted, "acquired out of address order");
        let listed: Vec<SocketAddr> = registry.targets().iter().map(|t| t.local_addr()).collect();
        assert!(listed.len() >= 3, "got {listed:?}");
        assert_eq!(listed, sorted, "all four, in address order");

        held[0].live.store(false, Ordering::Relaxed);
        drop(held.remove(1));
        let mut live = vec![held[1].local_addr(), held[2].local_addr()];
        live.sort();
        let listed: Vec<SocketAddr> = registry.targets().iter().map(|t| t.local_addr()).collect();
        assert_eq!(listed, live, "neither the stopped nor the dropped target");
    }

    /// An acquire waits for the socket of a target whose last handle dropped
    /// to close before it binds the address again, and binds anyway after
    /// two seconds.
    #[tokio::test(start_paused = true)]
    async fn an_acquire_waits_for_a_stopped_socket_to_close() {
        let registry = ListenerRegistry::<Fake>::new();
        let held_open = |listen: &str| FakeConfig {
            closes_on_drop: false,
            ..FakeConfig::new(listen, 1)
        };
        let old = acquire(&registry, held_open("127.0.0.1:0"))
            .await
            .expect("bind");
        let addr = old.local_addr();
        let socket = Arc::clone(&old.socket);
        drop(old);
        assert!(registry.is_empty(), "the dropped target is not live");
        assert!(*socket.borrow(), "its socket is still open");

        let closer = Arc::clone(&socket);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            closer.send_replace(false);
        });
        let started = tokio::time::Instant::now();
        let fresh = acquire(&registry, held_open(&addr.to_string()))
            .await
            .expect("binds once the socket closed");
        assert!(!*socket.borrow(), "bound only after the socket closed");
        assert!(started.elapsed() >= Duration::from_millis(300));

        // A socket that never closes delays the bind by two seconds at most.
        let never_closed = Arc::clone(&fresh.socket);
        drop(fresh);
        assert!(*never_closed.borrow(), "its socket is still open");
        let started = tokio::time::Instant::now();
        acquire(&registry, held_open(&addr.to_string()))
            .await
            .expect("binds after the wait runs out");
        assert_eq!(started.elapsed(), RELEASE_TIMEOUT);
    }

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
    #[case::v4_mapped_and_v4("[::ffff:127.0.0.1]:57400", "127.0.0.1:57400", true)]
    #[case::v4_mapped_and_v4_wildcard("[::ffff:127.0.0.1]:57400", "0.0.0.0:57400", true)]
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
}
