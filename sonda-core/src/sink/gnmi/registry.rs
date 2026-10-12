//! The process-wide map from listen address to live [`GnmiTarget`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use super::target::GnmiTarget;
use super::{GnmiTargetConfig, TargetSettings};
use crate::{ConfigError, SondaError};

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
/// server task exits; the stale entry is pruned on the next acquire.
pub struct GnmiTargetRegistry {
    inner: Mutex<HashMap<SocketAddr, Weak<GnmiTarget>>>,
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
        let target = GnmiTarget::bind(settings).await?;
        let mut map = self.map();
        map.retain(|_, target| target.strong_count() > 0);
        map.insert(target.local_addr(), Arc::downgrade(&target));
        Ok(target)
    }

    /// Live targets.
    pub fn len(&self) -> usize {
        self.map()
            .values()
            .filter(|target| target.strong_count() > 0)
            .count()
    }

    /// Whether no target is live.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn live(&self, addr: SocketAddr) -> Option<Arc<GnmiTarget>> {
        self.map().get(&addr).and_then(Weak::upgrade)
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<SocketAddr, Weak<GnmiTarget>>> {
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
}
