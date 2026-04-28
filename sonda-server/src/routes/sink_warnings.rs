//! Sink loopback pre-flight warnings shared by `POST /scenarios` and
//! `POST /events`.
//!
//! When a sink URL points at `localhost`, `127.0.0.1`, or `::1`, the
//! request still launches — these warnings exist so operators can spot
//! the misconfiguration in containerized deployments where loopback
//! resolves to the server's own network namespace, not the operator's
//! host.
//!
//! All helpers here are `pub(crate)` and have no side effects beyond
//! formatting strings (the logging helper writes to `tracing::warn`).

use sonda_core::config::ScenarioEntry;
use sonda_core::sink::SinkConfig;
use tracing::warn;

/// Pointer appended to every loopback warning so operators can find the
/// deployment networking reference without grepping docs.
pub(crate) const LOOPBACK_HINT_DOC: &str = "See docs/deployment/endpoints.md.";

/// Hosts treated as loopback by [`is_loopback_host`].
pub(crate) const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// Returns `true` when `host` is one of the canonical loopback names
/// (case-insensitive).
pub(crate) fn is_loopback_host(host: &str) -> bool {
    LOOPBACK_HOSTS
        .iter()
        .any(|candidate| host.eq_ignore_ascii_case(candidate))
}

/// Extract the host from a URL or `host:port` authority. Returns `None`
/// for unparseable input — sink construction will surface the real error.
pub(crate) fn extract_host(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip a URL scheme (`http://`, `https://`, `grpc://`, ...) if present.
    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };

    // Drop any path / query / fragment tail so we only parse the authority.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];

    // Strip userinfo (`user:pass@host`) if present.
    let authority = match authority.rfind('@') {
        Some(idx) => &authority[idx + 1..],
        None => authority,
    };

    if authority.is_empty() {
        return None;
    }

    // IPv6 literal: `[::1]` optionally followed by `:port`.
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.find(']').map(|end| &rest[..end]);
    }

    // IPv4 / hostname: split on the last `:` to drop the port, if any.
    let host = match authority.rfind(':') {
        Some(idx) => &authority[..idx],
        None => authority,
    };

    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Format the operator-facing warning string for a single offending sink.
pub(crate) fn format_loopback_warning(entry_name: &str, sink_tag: &str, offender: &str) -> String {
    format!(
        "scenario entry '{entry_name}' sink `{sink_tag}` targets `{offender}` — this host \
         resolves to the sonda-server container's own loopback, not your host. Use a Docker \
         Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name \
         instead. {LOOPBACK_HINT_DOC}"
    )
}

/// Inspect every entry's sink and return one warning string per loopback
/// target. Stdout/File/Channel/Memory sinks (and `*Disabled` placeholders)
/// produce no warnings.
pub(crate) fn sink_loopback_warnings(entries: &[ScenarioEntry]) -> Vec<String> {
    let mut warnings = Vec::new();
    for entry in entries {
        let base = entry.base();
        let name = base.name.as_str();
        collect_warnings_for_sink(&base.sink, name, &mut warnings);
    }
    warnings
}

/// Inspect a single sink config and append any loopback warnings it
/// generates to `out`.
///
/// Used directly by `POST /events`, which has a single sink rather than
/// a list of entries.
pub(crate) fn collect_warnings_for_sink(
    sink: &SinkConfig,
    entry_name: &str,
    out: &mut Vec<String>,
) {
    match sink {
        #[cfg(feature = "http")]
        SinkConfig::HttpPush { url, .. } => {
            if let Some(host) = extract_host(url) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "http_push", url));
                }
            }
        }
        #[cfg(feature = "http")]
        SinkConfig::Loki { url, .. } => {
            if let Some(host) = extract_host(url) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "loki", url));
                }
            }
        }
        #[cfg(feature = "remote-write")]
        SinkConfig::RemoteWrite { url, .. } => {
            if let Some(host) = extract_host(url) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "remote_write", url));
                }
            }
        }
        #[cfg(feature = "otlp")]
        SinkConfig::OtlpGrpc { endpoint, .. } => {
            if let Some(host) = extract_host(endpoint) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "otlp_grpc", endpoint));
                }
            }
        }
        #[cfg(feature = "kafka")]
        SinkConfig::Kafka { brokers, .. } => {
            // brokers is comma-separated; warn per loopback entry, not per sink.
            for broker in brokers.split(',') {
                let broker = broker.trim();
                if broker.is_empty() {
                    continue;
                }
                if let Some(host) = extract_host(broker) {
                    if is_loopback_host(host) {
                        out.push(format_loopback_warning(entry_name, "kafka", broker));
                    }
                }
            }
        }
        SinkConfig::Tcp { address, .. } => {
            if let Some(host) = extract_host(address) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "tcp", address));
                }
            }
        }
        SinkConfig::Udp { address } => {
            if let Some(host) = extract_host(address) {
                if is_loopback_host(host) {
                    out.push(format_loopback_warning(entry_name, "udp", address));
                }
            }
        }
        // Stdout/File/Channel/Memory carry no address; `*Disabled` placeholders
        // and future `#[non_exhaustive]` variants also land here.
        _ => {}
    }
}

/// Emit one `tracing::warn` per warning string, tagged with the route
/// label so operators can grep logs by endpoint.
pub(crate) fn log_warnings(route: &str, warnings: &[String]) {
    for message in warnings {
        warn!(message = %message, route = %route, "{}: sink pre-flight warning", route);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compile_scenario_file;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    // ---- is_loopback_host ----------------------------------------------------

    #[test]
    fn is_loopback_host_matches_canonical_hosts() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn is_loopback_host_is_case_insensitive() {
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("LocalHost"));
    }

    #[test]
    fn is_loopback_host_rejects_real_hostnames() {
        assert!(!is_loopback_host("victoriametrics"));
        assert!(!is_loopback_host("loki"));
        assert!(!is_loopback_host("10.0.0.1"));
        assert!(!is_loopback_host("192.168.1.10"));
        // 127/8 non-exact addresses are deliberately NOT matched per the
        // canonical-only policy.
        assert!(!is_loopback_host("127.0.0.2"));
    }

    // ---- extract_host --------------------------------------------------------

    #[test]
    fn extract_host_parses_http_url() {
        assert_eq!(
            extract_host("http://localhost:8428/api/v1/write"),
            Some("localhost")
        );
        assert_eq!(
            extract_host("https://victoriametrics:8428/write"),
            Some("victoriametrics")
        );
    }

    #[test]
    fn extract_host_parses_url_without_port() {
        assert_eq!(extract_host("http://localhost/push"), Some("localhost"));
    }

    #[test]
    fn extract_host_parses_bare_authority() {
        assert_eq!(extract_host("localhost:9094"), Some("localhost"));
        assert_eq!(
            extract_host("broker.example.com:9092"),
            Some("broker.example.com")
        );
    }

    #[test]
    fn extract_host_parses_ipv6_literal() {
        assert_eq!(extract_host("http://[::1]:8428/write"), Some("::1"));
        assert_eq!(extract_host("[::1]:9000"), Some("::1"));
        assert_eq!(
            extract_host("http://[2001:db8::1]/push"),
            Some("2001:db8::1")
        );
    }

    #[test]
    fn extract_host_handles_userinfo() {
        assert_eq!(
            extract_host("http://user:pass@localhost:8428/write"),
            Some("localhost")
        );
    }

    #[test]
    fn extract_host_rejects_empty_input() {
        assert_eq!(extract_host(""), None);
        assert_eq!(extract_host("   "), None);
    }

    // ---- sink_loopback_warnings (entry-list path used by /scenarios) --------

    /// Build a minimal v2 YAML body with the given sink block injected into
    /// `defaults` and compile it to a single `ScenarioEntry`. Used by the
    /// loopback pre-flight tests so we exercise the real compiler path.
    fn compile_single_entry_with_sink(sink_yaml: &str) -> ScenarioEntry {
        let yaml = format!(
            "version: 2\n\
             defaults:\n\
             \x20\x20rate: 10\n\
             \x20\x20duration: 500ms\n\
             \x20\x20encoder:\n\
             \x20\x20\x20\x20type: prometheus_text\n\
             {sink_yaml}\n\
             scenarios:\n\
             \x20\x20- id: loopback_test\n\
             \x20\x20\x20\x20signal_type: metrics\n\
             \x20\x20\x20\x20name: loopback_test\n\
             \x20\x20\x20\x20generator:\n\
             \x20\x20\x20\x20\x20\x20type: constant\n\
             \x20\x20\x20\x20\x20\x20value: 1.0\n"
        );
        let resolver = InMemoryPackResolver::new();
        let mut entries = compile_scenario_file(&yaml, &resolver).expect("compile must succeed");
        assert_eq!(entries.len(), 1, "test fixture must compile to one entry");
        entries.pop().unwrap()
    }

    #[test]
    fn sink_loopback_warnings_flags_tcp_localhost() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: tcp\n\x20\x20\x20\x20address: localhost:9000",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("loopback_test"));
        assert!(warnings[0].contains("tcp"));
        assert!(warnings[0].contains("localhost:9000"));
        assert!(warnings[0].contains("deployment/endpoints"));
    }

    #[test]
    fn sink_loopback_warnings_flags_udp_127_0_0_1() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: udp\n\x20\x20\x20\x20address: 127.0.0.1:9000",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("udp"));
        assert!(warnings[0].contains("127.0.0.1:9000"));
    }

    #[test]
    fn sink_loopback_warnings_skips_stdout() {
        let entry = compile_single_entry_with_sink("\x20\x20sink:\n\x20\x20\x20\x20type: stdout");
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert!(warnings.is_empty());
    }

    #[test]
    fn sink_loopback_warnings_skips_real_tcp_host() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: tcp\n\x20\x20\x20\x20address: syslog.example.com:514",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert!(warnings.is_empty());
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_loopback_warnings_flags_http_push_localhost() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: http_push\n\x20\x20\x20\x20url: http://localhost:8428/api/v1/write",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("http_push"));
        assert!(warnings[0].contains("http://localhost:8428/api/v1/write"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_loopback_warnings_flags_http_push_ipv6_loopback() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: http_push\n\x20\x20\x20\x20url: http://[::1]:8428/api/v1/write",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("[::1]"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn sink_loopback_warnings_skips_http_push_service_name() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: http_push\n\x20\x20\x20\x20url: http://victoriametrics:8428/api/v1/write",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert!(warnings.is_empty());
    }

    #[cfg(feature = "remote-write")]
    #[test]
    fn sink_loopback_warnings_flags_remote_write_localhost() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: remote_write\n\x20\x20\x20\x20url: http://localhost:8428/api/v1/write",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("remote_write"));
    }

    #[cfg(feature = "kafka")]
    #[test]
    fn sink_loopback_warnings_flags_one_localhost_broker_in_mixed_list() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: kafka\n\
             \x20\x20\x20\x20brokers: \"localhost:9094,real-broker:9092\"\n\
             \x20\x20\x20\x20topic: logs",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("kafka"));
        assert!(warnings[0].contains("localhost:9094"));
        assert!(!warnings[0].contains("real-broker"));
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn sink_loopback_warnings_flags_otlp_grpc_localhost() {
        let entry = compile_single_entry_with_sink(
            "\x20\x20sink:\n\x20\x20\x20\x20type: otlp_grpc\n\
             \x20\x20\x20\x20endpoint: http://localhost:4317\n\
             \x20\x20\x20\x20signal_type: metrics",
        );
        let warnings = sink_loopback_warnings(std::slice::from_ref(&entry));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("otlp_grpc"));
        assert!(warnings[0].contains("http://localhost:4317"));
    }

    // ---- collect_warnings_for_sink (single-sink path used by /events) -------

    #[test]
    fn collect_warnings_for_sink_flags_tcp_localhost() {
        let sink = SinkConfig::Tcp {
            address: "127.0.0.1:9000".to_string(),
            retry: None,
        };
        let mut out = Vec::new();
        collect_warnings_for_sink(&sink, "events", &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("'events'"));
        assert!(out[0].contains("tcp"));
    }

    #[test]
    fn collect_warnings_for_sink_skips_stdout() {
        let sink = SinkConfig::Stdout;
        let mut out = Vec::new();
        collect_warnings_for_sink(&sink, "events", &mut out);
        assert!(out.is_empty());
    }
}
