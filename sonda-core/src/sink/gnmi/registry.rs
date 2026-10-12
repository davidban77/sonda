//! The process-wide map from listen address to live [`GnmiTarget`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use tokio::sync::watch;

use super::target::{GnmiTarget, TargetState};
use super::{GnmiTargetConfig, TargetSettings};
use crate::{ConfigError, SondaError};

/// Longest an acquire waits for a stopped target's listener on the same port
/// to close before binding anyway.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// One registered target: a weak handle, and its listener state, which
/// outlives the target until the listener has closed.
struct Entry {
    target: Weak<GnmiTarget>,
    state: watch::Receiver<TargetState>,
}

impl Entry {
    /// The target, while a handle to it exists and its listener task runs.
    fn live(&self) -> Option<Arc<GnmiTarget>> {
        self.target.upgrade().filter(|target| target.is_serving())
    }

    /// Whether the target is not live but its listening socket may still be
    /// open.
    fn releasing(&self) -> bool {
        self.live().is_none() && *self.state.borrow() == TargetState::Listening
    }
}

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
pub struct GnmiTargetRegistry {
    inner: Mutex<HashMap<SocketAddr, Entry>>,
    /// Held across a bind so concurrent acquires of one address bind once.
    /// The map lock above is never held across an `.await`.
    binding: tokio::sync::Mutex<()>,
}

impl Default for GnmiTargetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GnmiTargetRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            binding: tokio::sync::Mutex::new(()),
        }
    }

    /// The live target for `cfg.listen`, binding and starting one if there
    /// is none. A listen address with port 0 always binds a new target.
    /// A target whose listener task has exited is not live, even while
    /// handles to it remain. Before binding, waits up to two seconds for the
    /// listening socket of any target on the same port that is no longer
    /// live to close.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, or when a live target on
    /// the address was bound with different settings; [`SondaError::Sink`]
    /// when the address cannot be bound.
    pub async fn acquire(&self, cfg: &GnmiTargetConfig) -> Result<Arc<GnmiTarget>, SondaError> {
        let settings = cfg.settings()?;
        let _binding = self.binding.lock().await;
        if settings.listen.port() != 0 {
            if let Some(target) = self.live(settings.listen) {
                return check_settings(target, &settings);
            }
        }
        if settings.listen.port() != 0 {
            self.released(settings.listen.port()).await;
        }
        let target = GnmiTarget::bind(settings).await?;
        let mut map = self.map();
        map.retain(|_, entry| entry.live().is_some() || entry.releasing());
        map.insert(
            target.local_addr(),
            Entry {
                target: Arc::downgrade(&target),
                state: target.state(),
            },
        );
        Ok(target)
    }

    /// The live target for `cfg.listen`, without ever binding.
    ///
    /// # Errors
    ///
    /// [`SondaError::Config`] for an invalid config, when no live target
    /// listens on the address, or when the live target was bound with
    /// different settings.
    pub fn join(&self, cfg: &GnmiTargetConfig) -> Result<Arc<GnmiTarget>, SondaError> {
        let settings = cfg.settings()?;
        let target = self
            .live(settings.listen)
            .filter(|_| settings.listen.port() != 0)
            .ok_or_else(|| {
                SondaError::Config(ConfigError::invalid(format!(
                    "no running scenario listens on {}",
                    settings.listen
                )))
            })?;
        check_settings(target, &settings)
    }

    /// Wait, up to [`RELEASE_TIMEOUT`], until no target that is no longer
    /// live still listens on `port`.
    async fn released(&self, port: u16) {
        let releasing: Vec<watch::Receiver<TargetState>> = self
            .map()
            .iter()
            .filter(|(addr, entry)| addr.port() == port && entry.releasing())
            .map(|(_, entry)| entry.state.clone())
            .collect();
        for mut state in releasing {
            let closed = state.wait_for(|s| *s != TargetState::Listening);
            let _ = tokio::time::timeout(RELEASE_TIMEOUT, closed).await;
        }
    }

    /// Live targets.
    pub fn len(&self) -> usize {
        self.map()
            .values()
            .filter(|entry| entry.live().is_some())
            .count()
    }

    /// Whether no target is live.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn live(&self, addr: SocketAddr) -> Option<Arc<GnmiTarget>> {
        self.map().get(&addr).and_then(Entry::live)
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<SocketAddr, Entry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn check_settings(
    target: Arc<GnmiTarget>,
    wanted: &TargetSettings,
) -> Result<Arc<GnmiTarget>, SondaError> {
    let running = target.settings();
    if running.serves(wanted) {
        return Ok(target);
    }
    Err(SondaError::Config(ConfigError::invalid(format!(
        "gnmi_target {} is already serving with max_subscribers {}, subscriber_buffer {} \
         and sample_floor {:?}; every sink on one address must use the same settings, \
         but this one asks for {}, {} and {:?}",
        target.local_addr(),
        running.max_subscribers,
        running.subscriber_buffer,
        running.sample_floor,
        wanted.max_subscribers,
        wanted.subscriber_buffer,
        wanted.sample_floor,
    ))))
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

        let mut sink = GnmiTargetSink::from_target(old);
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
}
