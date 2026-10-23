//! The process-wide map from listen address to live [`GnmiTarget`]: the
//! gnmi instance of [`ListenerRegistry`].

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use super::target::GnmiTarget;
use super::GnmiTargetConfig;
use crate::sink::listener_registry::ListenerRegistry;
use crate::SondaError;

/// The registry every [`GnmiTargetSink`](super::GnmiTargetSink) acquires its
/// target from. Created empty on first use; nothing binds until the first
/// acquire.
pub fn registry() -> &'static GnmiTargetRegistry {
    static REGISTRY: OnceLock<GnmiTargetRegistry> = OnceLock::new();
    REGISTRY.get_or_init(GnmiTargetRegistry::new)
}

/// Shares one [`GnmiTarget`] between every acquire that names its address.
///
/// The map holds weak handles, so a target lives exactly as long as some
/// sink holds it. Dropping the last handle cancels the target's token and its
/// listening socket closes; an acquire of the same port waits for that
/// before binding, and the stale entry is pruned once it has.
pub struct GnmiTargetRegistry(ListenerRegistry<GnmiTarget>);

impl Default for GnmiTargetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GnmiTargetRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self(ListenerRegistry::new())
    }

    /// The live target for `cfg.listen`, binding and starting one if there
    /// is none. A listen address with port 0 always binds a new target.
    /// A target whose listener task has exited is not live, even while
    /// handles to it remain. Before binding, waits up to two seconds in all
    /// for the listening socket of any target that is no longer live, on an
    /// address that overlaps this one, to close.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, when a live target on
    /// the address was bound with different settings, or when a live target
    /// on another address contends for the same port (a wildcard address and
    /// an address it covers); [`SondaError::Sink`] when the address cannot
    /// be bound.
    pub async fn acquire(&self, cfg: &GnmiTargetConfig) -> Result<Arc<GnmiTarget>, SondaError> {
        let settings = cfg.settings()?;
        let listen: SocketAddr = settings.listen;
        self.0.acquire(settings, listen).await
    }

    /// Whether [`acquire`](Self::acquire) of `cfg` would be refused by the
    /// targets running now, without binding anything: the config is invalid,
    /// a live target on the address runs with different settings, or a live
    /// target on another address contends for the port.
    ///
    /// # Errors
    ///
    /// The [`SondaError::Config`] that `acquire` would return for those
    /// cases.
    pub fn check(&self, cfg: &GnmiTargetConfig) -> Result<(), SondaError> {
        let settings = cfg.settings()?;
        self.0.check(&settings, settings.listen)
    }

    /// The live target that serves `cfg.listen`, without ever binding: the
    /// target on that address, or a wildcard target on the same port that
    /// covers it. Only the settings `cfg` sets are compared with the
    /// target's; the others are left to the running target.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, when no live target
    /// serves the address, or when a setting `cfg` sets differs from the
    /// target's.
    pub fn join(&self, cfg: &GnmiTargetConfig) -> Result<Arc<GnmiTarget>, SondaError> {
        let settings = cfg.settings()?;
        self.0.join(settings.listen, |target| {
            let mut wanted = target.settings().clone();
            if cfg.max_subscribers.is_some() {
                wanted.max_subscribers = settings.max_subscribers;
            }
            if cfg.subscriber_buffer.is_some() {
                wanted.subscriber_buffer = settings.subscriber_buffer;
            }
            if cfg.sample_floor.is_some() {
                wanted.sample_floor = settings.sample_floor;
            }
            if cfg.tls.is_some() {
                wanted.tls = settings.tls;
            }
            if cfg.credentials.is_some() {
                wanted.credentials = settings.credentials;
            }
            wanted
        })
    }

    /// Live targets.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no target is live.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The live targets, sorted by the address each listens on.
    pub fn targets(&self) -> Vec<Arc<GnmiTarget>> {
        self.0.targets()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::sink::gnmi::GnmiTargetSink;

    /// Each test uses its own registry so targets other tests bind cannot
    /// change the counts asserted here.
    #[tokio::test]
    async fn same_address_shares_one_target() {
        let registry = GnmiTargetRegistry::new();
        let first = registry
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr();
        assert_ne!(addr.port(), 0, "the bound port is read back");

        let second = registry
            .acquire(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .expect("join the live target");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(registry.len(), 1);
    }

    /// `targets()` lists the live targets in address order and leaves out
    /// one whose listener stopped and one whose last handle dropped.
    #[tokio::test]
    async fn targets_are_sorted_by_address_and_only_live() {
        let registry = GnmiTargetRegistry::new();
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(
                registry
                    .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
                    .await
                    .expect("bind an ephemeral port"),
            );
        }
        let mut addrs: Vec<SocketAddr> = held.iter().map(|t| t.local_addr()).collect();
        addrs.sort();
        let listed: Vec<SocketAddr> = registry.targets().iter().map(|t| t.local_addr()).collect();
        assert_eq!(listed, addrs, "all three, in address order");

        let stopped = held.pop().expect("three held");
        stopped.stop_serving();
        let dropped = held.pop().expect("two held");
        let dropped_addr = dropped.local_addr();
        drop(dropped);
        let live = held[0].local_addr();
        let deadline = Instant::now() + Duration::from_secs(2);
        while registry.targets().len() != 1 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let listed: Vec<SocketAddr> = registry.targets().iter().map(|t| t.local_addr()).collect();
        assert_eq!(
            listed,
            [live],
            "neither {} nor {dropped_addr}",
            stopped.local_addr()
        );
    }

    #[tokio::test]
    async fn conflicting_config_is_rejected() {
        let registry = GnmiTargetRegistry::new();
        let first = registry
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let mut other = GnmiTargetConfig::new(first.local_addr().to_string());
        other.max_subscribers = Some(3);

        let err = registry
            .acquire(&other)
            .await
            .err()
            .expect("different settings on one address must be refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(err.to_string().contains("max_subscribers 16"), "got {err}");
        assert_eq!(registry.len(), 1, "the live target is untouched");
    }

    #[tokio::test]
    async fn last_drop_closes_the_port() {
        let started = Instant::now();
        let first = GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.target().local_addr();
        let second = GnmiTargetSink::new(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .expect("join the live target");
        assert!(Arc::ptr_eq(first.target(), second.target()));
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("the port accepts while a sink lives");

        let stopped = first.target().wait();
        drop(first);
        drop(second);
        tokio::time::timeout(Duration::from_secs(1), stopped)
            .await
            .expect("the listener stops within 1 s of the last drop");
        let refused =
            tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr))
                .await;
        assert!(
            matches!(refused, Ok(Err(_)) | Err(_)),
            "a connect after the last drop must fail, got {refused:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "teardown took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn invalid_config_binds_nothing() {
        let registry = GnmiTargetRegistry::new();
        let err = registry
            .acquire(&GnmiTargetConfig::new("not-an-address"))
            .await
            .err()
            .expect("an unparsable listen address is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(registry.is_empty());
    }

    /// Dropping the last handle and acquiring the same address straight away
    /// binds again: the acquire waits for the old listener to close.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reacquire_immediately_after_last_drop_succeeds() {
        let registry = GnmiTargetRegistry::new();
        let first = registry
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr();
        drop(first);

        let mut bound = 0usize;
        for attempt in 0..200 {
            let target = registry
                .acquire(&GnmiTargetConfig::new(addr.to_string()))
                .await
                .unwrap_or_else(|e| panic!("attempt {attempt} could not re-acquire {addr}: {e}"));
            assert_eq!(target.local_addr(), addr);
            tokio::net::TcpStream::connect(addr)
                .await
                .unwrap_or_else(|e| panic!("attempt {attempt}: nothing listens on {addr}: {e}"));
            bound += 1;
            drop(target);
        }
        assert_eq!(bound, 200);
    }

    /// A target whose listener task has exited is not live: the next acquire
    /// binds a new one, and a write through the old handle fails.
    #[tokio::test]
    async fn stopped_serve_task_counts_as_dead() {
        let registry = GnmiTargetRegistry::new();
        let old = registry
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = old.local_addr();
        old.check_serving().expect("a new target serves");
        assert_eq!(registry.len(), 1);

        old.stop_serving();
        tokio::time::timeout(Duration::from_secs(2), old.wait())
            .await
            .expect("the listener task exits");
        assert!(!old.is_serving());
        assert_eq!(registry.len(), 0, "a stopped target is not live");

        let fresh = registry
            .acquire(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .expect("a fresh target binds the address");
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert!(fresh.is_serving());
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("the fresh target listens");

        let mut sink = GnmiTargetSink::from_target(old, crate::schedule::LaunchId::next());
        let event = crate::model::metric::MetricEvent::new(
            "up".to_string(),
            1.0,
            crate::model::metric::Labels::default(),
        )
        .expect("valid event");
        let err = crate::sink::Sink::write_metric_event(&mut sink, &event, b"x")
            .await
            .expect_err("a write through the stopped target fails");
        assert!(err.to_string().contains("no longer serving"), "got {err}");
    }

    /// A target whose listener task was dropped with its runtime is not
    /// live, though a handle to it is still held: join refuses it, the next
    /// acquire on another runtime binds the address afresh, and a write
    /// through the old handle fails.
    #[test]
    fn a_target_whose_runtime_shut_down_is_not_live() {
        let registry = GnmiTargetRegistry::new();
        let first = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let old = first
            .block_on(registry.acquire(&GnmiTargetConfig::new("127.0.0.1:0")))
            .expect("bind an ephemeral port");
        let addr = old.local_addr();
        assert!(old.is_serving());
        // A connection accepted by the listener task proves the task ran.
        let client = first
            .block_on(async {
                let client = tokio::net::TcpStream::connect(addr).await?;
                tokio::task::yield_now().await;
                Ok::<_, std::io::Error>(client)
            })
            .expect("the target accepts");
        drop(first);
        drop(client);

        assert!(!old.is_serving(), "the listener task went with its runtime");
        assert_eq!(registry.len(), 0, "a target nobody serves is not live");
        assert!(registry
            .join(&GnmiTargetConfig::new(addr.to_string()))
            .is_err());
        let err = old.check_serving().expect_err("the old handle is dead");
        assert!(err.to_string().contains("no longer serving"), "got {err}");

        let second = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        second.block_on(async {
            let fresh = tokio::time::timeout(
                Duration::from_secs(1),
                registry.acquire(&GnmiTargetConfig::new(addr.to_string())),
            )
            .await
            .expect("the dead target's socket is already closed")
            .expect("a fresh target binds the address");
            assert!(!Arc::ptr_eq(&old, &fresh));
            tokio::net::TcpStream::connect(addr)
                .await
                .expect("the fresh target listens");
        });
    }

    #[tokio::test]
    async fn join_returns_the_live_target_and_never_binds() {
        let registry = GnmiTargetRegistry::new();
        let held = registry
            .acquire(&GnmiTargetConfig::new("127.0.0.1:0"))
            .await
            .expect("bind an ephemeral port");
        let addr = held.local_addr();
        let joined = registry
            .join(&GnmiTargetConfig::new(addr.to_string()))
            .expect("a live target is joined");
        assert!(Arc::ptr_eq(&held, &joined));

        let mut other = GnmiTargetConfig::new(addr.to_string());
        other.max_subscribers = Some(3);
        let err = registry.join(&other).err().expect("settings must match");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");

        drop((held, joined));
        let err = registry
            .join(&GnmiTargetConfig::new(addr.to_string()))
            .err()
            .expect("nothing to join once the last handle is gone");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(
            err.to_string().contains("no running scenario listens on"),
            "got {err}"
        );
        assert!(registry.is_empty(), "join bound nothing");

        let err = registry
            .join(&GnmiTargetConfig::new("127.0.0.1:0"))
            .err()
            .expect("port 0 names no running target");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(registry.is_empty());
    }

    /// A join compares only the settings it sets: a request that names just
    /// the address joins a target running with non-default settings.
    #[tokio::test]
    async fn join_compares_only_the_settings_it_sets() {
        let registry = GnmiTargetRegistry::new();
        let mut running = GnmiTargetConfig::new("127.0.0.1:0");
        running.max_subscribers = Some(4);
        let held = registry
            .acquire(&running)
            .await
            .expect("bind an ephemeral port");
        let addr = held.local_addr().to_string();

        let joined = registry
            .join(&GnmiTargetConfig::new(addr.clone()))
            .expect("an unset field defers to the running target");
        assert!(Arc::ptr_eq(&held, &joined));

        let mut same = GnmiTargetConfig::new(addr.clone());
        same.max_subscribers = Some(4);
        registry.join(&same).expect("a matching field joins");

        let mut other = GnmiTargetConfig::new(addr);
        other.max_subscribers = Some(8);
        let err = registry
            .join(&other)
            .err()
            .expect("a differing field is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        assert!(err.to_string().contains("max_subscribers 4"), "got {err}");
    }

    /// A live wildcard target serves a specific address on its port: a join
    /// of that address finds it, and an acquire of it is a config conflict
    /// naming the running target instead of a bind error.
    #[tokio::test]
    async fn a_wildcard_target_serves_the_addresses_it_covers() {
        let registry = GnmiTargetRegistry::new();
        // A wildcard listener is the subject of this test; its port is
        // ephemeral and read back.
        let wild = registry
            .acquire(&GnmiTargetConfig::new("0.0.0.0:0"))
            .await
            .expect("bind an ephemeral wildcard port");
        let port = wild.local_addr().port();
        let specific = format!("127.0.0.1:{port}");

        let joined = registry
            .join(&GnmiTargetConfig::new(specific.clone()))
            .expect("the wildcard target serves 127.0.0.1");
        assert!(Arc::ptr_eq(&wild, &joined));
        assert!(
            registry
                .join(&GnmiTargetConfig::new(format!("[::]:{port}")))
                .is_err(),
            "an IPv4 wildcard does not serve the IPv6 wildcard"
        );

        let err = registry
            .acquire(&GnmiTargetConfig::new(specific))
            .await
            .err()
            .expect("a second listener on the port is refused");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains(&format!("0.0.0.0:{port}")), "got {msg}");
        let checked = registry
            .check(&GnmiTargetConfig::new(format!("127.0.0.1:{port}")))
            .expect_err("check refuses what acquire refuses");
        assert_eq!(checked.to_string(), msg);
        assert_eq!(registry.len(), 1, "nothing else was bound");
    }

    /// `check` refuses exactly what `acquire` would against the running
    /// targets, and binds nothing.
    #[tokio::test]
    async fn check_answers_like_acquire_without_binding() {
        let registry = GnmiTargetRegistry::new();
        let mut running = GnmiTargetConfig::new("127.0.0.1:0");
        running.max_subscribers = Some(4);
        let held = registry
            .acquire(&running)
            .await
            .expect("bind an ephemeral port");
        let addr = held.local_addr();

        let mut same = GnmiTargetConfig::new(addr.to_string());
        same.max_subscribers = Some(4);
        registry.check(&same).expect("matching settings join");

        let err = registry
            .check(&GnmiTargetConfig::new(addr.to_string()))
            .expect_err("the defaults differ from the running target");
        assert!(matches!(err, SondaError::Config(_)), "got {err:?}");
        let acquired = registry
            .acquire(&GnmiTargetConfig::new(addr.to_string()))
            .await
            .err()
            .expect("acquire refuses the same config");
        assert_eq!(err.to_string(), acquired.to_string());

        registry
            .check(&GnmiTargetConfig::new("127.0.0.1:0"))
            .expect("port 0 never conflicts");
        assert!(registry
            .check(&GnmiTargetConfig::new("not-an-address"))
            .is_err());
        assert_eq!(registry.len(), 1, "check bound nothing");
    }

    /// The config error `result` holds, as text.
    fn config_error<T>(result: Result<T, SondaError>) -> String {
        match result {
            Err(SondaError::Config(e)) => e.to_string(),
            Err(other) => panic!("a config error, got {other:?}"),
            Ok(_) => panic!("a config error, got a target"),
        }
    }

    /// Sinks on one address agree on `tls` and `credentials`: equal ones
    /// share the target, and `acquire`, `check` and `join` refuse other TLS
    /// paths or another password, naming neither password.
    #[tokio::test]
    async fn sinks_on_one_address_agree_on_tls_and_credentials() {
        use crate::sink::gnmi::test_tls::TlsFiles;
        use crate::sink::gnmi::{GnmiCredentials, GnmiTlsConfig};

        let files = TlsFiles::new();
        let cfg = |listen: &str, tls: GnmiTlsConfig, password: &str| GnmiTargetConfig {
            tls: Some(tls),
            credentials: Some(GnmiCredentials {
                username: "sonda".to_string(),
                password: password.to_string(),
            }),
            ..GnmiTargetConfig::new(listen)
        };
        let registry = GnmiTargetRegistry::new();
        let first = registry
            .acquire(&cfg("127.0.0.1:0", files.tls(), "first-secret"))
            .await
            .expect("bind an ephemeral port");
        let addr = first.local_addr().to_string();
        let shared = registry
            .acquire(&cfg(&addr, files.tls(), "first-secret"))
            .await
            .expect("equal settings share the target");
        assert!(Arc::ptr_eq(&first, &shared));

        let copy = files.write(
            "copy.pem",
            &std::fs::read_to_string(files.path("server.pem")).expect("the cert"),
        );
        let moved = GnmiTlsConfig {
            cert: copy.clone(),
            ..files.tls()
        };
        let other_tls = cfg(&addr, moved, "first-secret");
        let other_password = cfg(&addr, files.tls(), "second-secret");
        for refused in [
            config_error(registry.acquire(&other_tls).await),
            config_error(registry.check(&other_tls).map(|()| first.clone())),
            config_error(registry.join(&other_tls)),
        ] {
            assert!(refused.contains("different settings"), "{refused}");
            assert!(refused.contains(&format!("{copy:?}")), "{refused}");
            assert!(!refused.contains("first-secret"), "{refused}");
        }
        for refused in [
            config_error(registry.acquire(&other_password).await),
            config_error(registry.check(&other_password).map(|()| first.clone())),
            config_error(registry.join(&other_password)),
        ] {
            assert!(
                refused.contains(r#"username "sonda" and username "sonda" with another password"#),
                "{refused}"
            );
            assert!(
                !refused.contains("first-secret") && !refused.contains("second-secret"),
                "{refused}"
            );
        }
        assert_eq!(registry.len(), 1, "nothing else bound");
    }
}
