//! Shared sink display formatting for CLI output.
//!
//! Both the dry-run formatter ([`crate::dry_run`]) and the lifecycle banner
//! ([`crate::status`]) need to render a [`SinkConfig`] as a one-line label.
//! The dry-run uses the spec §5 format (`name (detail)`), and the banner
//! historically used a slightly different form (`name: detail`). PR 7's
//! reviewer flagged two issues:
//!
//! 1. Two parallel match expressions had drifted in formatting and one of
//!    them did not cover the feature-gated `*Disabled {}` variants, which
//!    broke `cargo build --no-default-features`.
//! 2. The duplicated logic was a maintenance hazard: a new `SinkConfig`
//!    variant would require updating two unrelated files.
//!
//! This module owns the canonical rendering and is the single source of
//! truth for both call sites. The format is the spec §5 form
//! (`name (detail)`) because that matches the dry-run output the spec
//! prescribes; the lifecycle banner adopts it for consistency.

use sonda_core::sink::SinkConfig;

/// Format a [`SinkConfig`] as a one-line human-readable label.
///
/// The output uses the spec §5 form:
///
/// - Sinks without configurable detail render as just their name
///   (`stdout`).
/// - Sinks with one piece of detail render as `name (detail)`
///   (`file (/tmp/out.txt)`, `tcp (127.0.0.1:9999)`).
/// - The Kafka sink renders as `kafka (brokers / topic)` because both
///   pieces of information are operationally relevant.
/// - When a sink's Cargo feature is disabled, the placeholder `Disabled`
///   variants render as `name (disabled)` so users can see the
///   configuration was accepted but cannot run.
///
/// This function is exhaustive over [`SinkConfig`] under every feature
/// combination, so adding a new variant in `sonda-core` will fail to
/// compile here until it is wired up — preventing the
/// `--no-default-features` regression that prompted this module.
pub fn sink_display(sink: &SinkConfig) -> String {
    match sink {
        SinkConfig::Stdout => "stdout".to_string(),
        SinkConfig::File { path } => format!("file ({path})"),
        SinkConfig::Tcp { address, .. } => format!("tcp ({address})"),
        SinkConfig::Udp { address } => format!("udp ({address})"),
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { url, .. } => format!("http_push ({url})"),
        #[cfg(not(feature = "http"))]
        SinkConfig::HttpPushDisabled {} => "http_push (disabled)".to_string(),
        #[cfg(feature = "http")]
        SinkConfig::Loki { url, .. } => format!("loki ({url})"),
        #[cfg(not(feature = "http"))]
        SinkConfig::LokiDisabled {} => "loki (disabled)".to_string(),
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, .. } => format!("remote_write ({url})"),
        #[cfg(not(feature = "remote-write"))]
        SinkConfig::RemoteWriteDisabled {} => "remote_write (disabled)".to_string(),
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { brokers, topic, .. } => format!("kafka ({brokers} / {topic})"),
        #[cfg(not(feature = "kafka"))]
        SinkConfig::KafkaDisabled {} => "kafka (disabled)".to_string(),
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc { endpoint, .. } => format!("otlp_grpc ({endpoint})"),
        #[cfg(not(feature = "otlp"))]
        SinkConfig::OtlpGrpcDisabled {} => "otlp_grpc (disabled)".to_string(),
        // `SinkConfig` is `#[non_exhaustive]` across the crate boundary;
        // fall back to the Debug form so a future variant still renders.
        other => format!("unknown ({other:?})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_renders_without_detail() {
        assert_eq!(sink_display(&SinkConfig::Stdout), "stdout");
    }

    #[test]
    fn file_includes_path_in_parens() {
        let s = SinkConfig::File {
            path: "/tmp/out.txt".to_string(),
        };
        assert_eq!(sink_display(&s), "file (/tmp/out.txt)");
    }

    #[test]
    fn tcp_includes_address_in_parens() {
        let s = SinkConfig::Tcp {
            address: "127.0.0.1:9999".to_string(),
            retry: None,
        };
        assert_eq!(sink_display(&s), "tcp (127.0.0.1:9999)");
    }

    #[test]
    fn udp_includes_address_in_parens() {
        let s = SinkConfig::Udp {
            address: "127.0.0.1:8888".to_string(),
        };
        assert_eq!(sink_display(&s), "udp (127.0.0.1:8888)");
    }

    #[cfg(feature = "http")]
    #[test]
    fn http_push_includes_url_in_parens() {
        let s = SinkConfig::HttpPush {
            url: "http://localhost:9090/write".to_string(),
            content_type: None,
            batch_size: None,
            max_buffer_age: None,
            headers: None,
            retry: None,
        };
        assert_eq!(sink_display(&s), "http_push (http://localhost:9090/write)");
    }

    #[cfg(not(feature = "http"))]
    #[test]
    fn http_push_disabled_renders_disabled_marker() {
        // SAFETY: the `Disabled` placeholder variants only exist when the
        // matching feature is off, so the test is naturally feature-gated.
        let s = SinkConfig::HttpPushDisabled {};
        assert_eq!(sink_display(&s), "http_push (disabled)");
    }

    #[cfg(not(feature = "http"))]
    #[test]
    fn loki_disabled_renders_disabled_marker() {
        let s = SinkConfig::LokiDisabled {};
        assert_eq!(sink_display(&s), "loki (disabled)");
    }
}
