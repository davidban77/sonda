//! The ureq agent the HTTP push sinks POST through.

use std::time::Duration;

/// The agent the `http_push`, `loki` and `remote_write` sinks POST through.
///
/// A 4xx or 5xx comes back as a response, whose status the sink classifies
/// itself. No proxy is read from the environment, connecting is bounded by
/// 30 s (the request as a whole is not), and at most 5 redirects are followed.
pub(crate) fn push_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_connect(Some(Duration::from_secs(30)))
        .max_redirects(5)
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_agent_reads_statuses_itself_and_bounds_only_the_connect() {
        let agent = push_agent();
        let config = agent.config();
        assert!(!config.http_status_as_error());
        assert!(config.proxy().is_none());
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(30)));
        assert_eq!(config.timeouts().global, None);
        assert_eq!(config.max_redirects(), 5);
    }
}
