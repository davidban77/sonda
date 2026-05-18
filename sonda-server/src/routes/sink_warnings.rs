//! Sink loopback pre-flight warnings shared by `POST /scenarios` and `POST /events`.

use sonda_core::config::{DynamicLabelConfig, DynamicLabelStrategy, ScenarioEntry};
use sonda_core::sink::SinkConfig;
use tracing::warn;

pub(crate) const LOOPBACK_HINT_DOC: &str = "See docs/deployment/endpoints.md.";

pub(crate) const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

pub(crate) fn is_loopback_host(host: &str) -> bool {
    LOOPBACK_HOSTS
        .iter()
        .any(|candidate| host.eq_ignore_ascii_case(candidate))
}

/// Pulls the host out of a URL or bare `host:port` authority.
pub(crate) fn extract_host(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };

    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];

    let authority = match authority.rfind('@') {
        Some(idx) => &authority[idx + 1..],
        None => authority,
    };

    if authority.is_empty() {
        return None;
    }

    if let Some(rest) = authority.strip_prefix('[') {
        return rest.find(']').map(|end| &rest[..end]);
    }

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

pub(crate) fn format_loopback_warning(entry_name: &str, sink_tag: &str, offender: &str) -> String {
    format!(
        "scenario entry '{entry_name}' sink `{sink_tag}` targets `{offender}` — this host \
         resolves to the sonda-server container's own loopback, not your host. Use a Docker \
         Compose service name (e.g. `victoriametrics:8428`) or a Kubernetes Service DNS name \
         instead. {LOOPBACK_HINT_DOC}"
    )
}

pub(crate) fn sink_loopback_warnings(entries: &[ScenarioEntry]) -> Vec<String> {
    let mut warnings = Vec::new();
    for entry in entries {
        let base = entry.base();
        let name = base.name.as_str();
        collect_warnings_for_sink(&base.sink, name, &mut warnings);
    }
    warnings
}

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
        _ => {}
    }
}

pub(crate) fn log_warnings(route: &str, warnings: &[String]) {
    for message in warnings {
        warn!(message = %message, route = %route, "{}: sink pre-flight warning", route);
    }
}

/// One informational warning per logs+loki+`dynamic_labels` entry, naming
/// the predicted upper bound on distinct Loki streams the scenario will
/// produce over its lifetime alongside the active `max_streams_per_push`
/// cap. Lets users see — at registration time — whether their cardinality
/// will fit before flushes start hitting the cap at runtime.
pub(crate) fn loki_cardinality_warnings(entries: &[ScenarioEntry]) -> Vec<String> {
    let mut warnings = Vec::new();
    for entry in entries {
        if !matches!(entry, ScenarioEntry::Logs(_)) {
            continue;
        }
        let base = entry.base();
        let Some(dyn_labels) = base.dynamic_labels.as_deref() else {
            continue;
        };
        if dyn_labels.is_empty() {
            continue;
        }
        if let Some(msg) = preview_loki_cardinality(&base.sink, &base.name, dyn_labels) {
            warnings.push(msg);
        }
    }
    warnings
}

fn preview_loki_cardinality(
    sink: &SinkConfig,
    entry_name: &str,
    dyn_labels: &[DynamicLabelConfig],
) -> Option<String> {
    match sink {
        #[cfg(feature = "http")]
        SinkConfig::Loki {
            max_streams_per_push,
            ..
        } => {
            let cap = max_streams_per_push
                .unwrap_or(sonda_core::sink::loki::DEFAULT_MAX_STREAMS_PER_PUSH);
            let predicted = predicted_loki_stream_count(dyn_labels);
            let keys: Vec<&str> = dyn_labels.iter().map(|dl| dl.key.as_str()).collect();
            Some(format!(
                "scenario entry '{entry_name}' will produce up to {predicted} distinct \
                 Loki streams (dynamic_labels: {}). max_streams_per_push is {cap}.",
                keys.join(", ")
            ))
        }
        _ => None,
    }
}

fn predicted_loki_stream_count(dyn_labels: &[DynamicLabelConfig]) -> u64 {
    dyn_labels
        .iter()
        .map(|dl| match &dl.strategy {
            DynamicLabelStrategy::Counter { cardinality, .. } => *cardinality,
            DynamicLabelStrategy::ValuesList { values } => values.len() as u64,
        })
        .fold(1u64, lcm)
}

fn lcm(a: u64, b: u64) -> u64 {
    if a == 0 || b == 0 {
        0
    } else {
        a / gcd(a, b) * b
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compile_scenario_file;
    use sonda_core::compiler::expand::InMemoryPackResolver;

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
        assert!(!is_loopback_host("127.0.0.2"));
    }

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

    fn compile_single_entry_with_sink(sink_yaml: &str) -> ScenarioEntry {
        let yaml = format!(
            "version: 2\n\
             kind: runnable\n\
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

    // ---- loki cardinality preview --------------------------------------

    fn compile_logs_entry(body_yaml: &str) -> ScenarioEntry {
        let yaml = format!(
            "version: 2\nkind: runnable\n\
             defaults:\n  rate: 10\n  duration: 500ms\n  encoder:\n    type: json_lines\n\
             scenarios:\n  - id: card_test\n    signal_type: logs\n    name: card_test\n\
{body_yaml}",
        );
        let resolver = InMemoryPackResolver::new();
        let mut entries = compile_scenario_file(&yaml, &resolver).expect("compile must succeed");
        assert_eq!(entries.len(), 1, "fixture must compile to one entry");
        entries.pop().unwrap()
    }

    #[cfg(feature = "http")]
    #[test]
    fn loki_cardinality_warning_fires_for_logs_loki_dynamic_labels() {
        let entry = compile_logs_entry(
            "    sink:\n      type: loki\n      url: http://loki:3100\n\
             \x20\x20\x20\x20dynamic_labels:\n      - key: peer_address\n\
             \x20\x20\x20\x20\x20\x20\x20\x20values: [\"10.1.2.2\", \"10.1.7.2\"]\n\
             \x20\x20\x20\x20log_generator:\n      type: template\n      templates:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20- message: \"hi\"\n",
        );
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("card_test"));
        assert!(out[0].contains("peer_address"));
        assert!(out[0].contains("up to 2 distinct"));
        assert!(out[0].contains("max_streams_per_push is 128"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn loki_cardinality_warning_uses_explicit_cap() {
        let entry = compile_logs_entry(
            "    sink:\n      type: loki\n      url: http://loki:3100\n\
             \x20\x20\x20\x20\x20\x20max_streams_per_push: 32\n\
             \x20\x20\x20\x20dynamic_labels:\n      - key: peer_address\n\
             \x20\x20\x20\x20\x20\x20\x20\x20values: [\"a\", \"b\", \"c\"]\n\
             \x20\x20\x20\x20log_generator:\n      type: template\n      templates:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20- message: \"hi\"\n",
        );
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("max_streams_per_push is 32"));
        assert!(out[0].contains("up to 3"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn loki_cardinality_warning_uses_lcm_for_multiple_dynamic_labels() {
        let entry = compile_logs_entry(
            "    sink:\n      type: loki\n      url: http://loki:3100\n\
             \x20\x20\x20\x20dynamic_labels:\n      - key: peer\n\
             \x20\x20\x20\x20\x20\x20\x20\x20values: [\"a\", \"b\", \"c\"]\n\
             \x20\x20\x20\x20\x20\x20- key: pod\n\
             \x20\x20\x20\x20\x20\x20\x20\x20values: [\"x\", \"y\"]\n\
             \x20\x20\x20\x20log_generator:\n      type: template\n      templates:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20- message: \"hi\"\n",
        );
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert_eq!(out.len(), 1);
        // LCM(3, 2) = 6 distinct (peer, pod) combinations.
        assert!(out[0].contains("up to 6 distinct"), "got: {}", out[0]);
    }

    #[cfg(feature = "http")]
    #[test]
    fn loki_cardinality_warning_does_not_fire_without_dynamic_labels() {
        let entry = compile_logs_entry(
            "    sink:\n      type: loki\n      url: http://loki:3100\n\
             \x20\x20\x20\x20log_generator:\n      type: template\n      templates:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20- message: \"hi\"\n",
        );
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert!(out.is_empty());
    }

    #[cfg(feature = "http")]
    #[test]
    fn loki_cardinality_warning_does_not_fire_for_non_loki_sink() {
        let entry = compile_logs_entry(
            "    sink:\n      type: stdout\n\
             \x20\x20\x20\x20dynamic_labels:\n      - key: peer\n\
             \x20\x20\x20\x20\x20\x20\x20\x20values: [\"a\", \"b\"]\n\
             \x20\x20\x20\x20log_generator:\n      type: template\n      templates:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20- message: \"hi\"\n",
        );
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert!(out.is_empty());
    }

    #[test]
    fn loki_cardinality_warning_does_not_fire_for_metrics_entry() {
        // metrics + dynamic_labels (no Loki sink possible for metrics in
        // practice) — the cardinality preview is logs-only.
        let yaml = "version: 2\nkind: runnable\n\
                    defaults:\n  rate: 10\n  duration: 500ms\n\
                    \x20\x20encoder:\n    type: prometheus_text\n\
                    scenarios:\n  - id: m\n    signal_type: metrics\n    name: m\n\
                    \x20\x20\x20\x20sink:\n      type: stdout\n\
                    \x20\x20\x20\x20generator:\n      type: constant\n      value: 1.0\n\
                    \x20\x20\x20\x20dynamic_labels:\n      - key: peer\n\
                    \x20\x20\x20\x20\x20\x20\x20\x20values: [\"a\", \"b\"]\n";
        let resolver = InMemoryPackResolver::new();
        let mut entries = compile_scenario_file(yaml, &resolver).expect("compile");
        let entry = entries.pop().unwrap();
        let out = loki_cardinality_warnings(std::slice::from_ref(&entry));
        assert!(out.is_empty());
    }
}
