//! Config helpers for the `sonda run` subcommand.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use sonda_core::encoder::EncoderConfig;
use sonda_core::sink::SinkConfig;

use crate::cli::RunArgs;

/// Resolve a `--scenario` argument string (path or `@name`) to a YAML string.
///
/// When the string starts with `@`, `catalog_dir` is required.
pub fn resolve_scenario_source(scenario_ref: &str, catalog_dir: Option<&Path>) -> Result<String> {
    if let Some(name) = scenario_ref.strip_prefix('@') {
        let dir = catalog_dir
            .ok_or_else(|| anyhow!("--catalog <dir> is required to resolve @name references"))?;
        let path = crate::catalog_dir::resolve(dir, name)?;
        fs::read_to_string(&path)
            .map_err(|e| anyhow!("failed to read catalog entry {}: {e}", path.display()))
    } else {
        let path = PathBuf::from(scenario_ref);
        fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "failed to read scenario file {}: {e}\n\n  hint: use `@name` for catalog entries (requires --catalog <dir>)",
                    path.display()
                )
            } else {
                anyhow!("failed to read scenario file {}: {e}", path.display())
            }
        })
    }
}

pub fn apply_run_overrides_compiled(
    file: &mut sonda_core::compiler::compile_after::CompiledFile,
    args: &RunArgs,
) -> Result<()> {
    let (sink_override, encoder_override) = resolve_run_overrides(args)?;
    for entry in file.entries.iter_mut() {
        if let Some(ref dur) = args.duration {
            entry.duration = Some(dur.clone());
        }
        if let Some(rate) = args.rate {
            entry.rate = rate;
        }
        if let Some(ref sink) = sink_override {
            entry.sink = sink.clone();
        }
        if let Some(policy) = args.on_sink_error {
            entry.on_sink_error = policy;
        }
        if !args.labels.is_empty() {
            let map = entry
                .labels
                .get_or_insert_with(std::collections::BTreeMap::new);
            for (k, v) in &args.labels {
                map.insert(k.clone(), v.clone());
            }
        }
        if let Some(ref enc) = encoder_override {
            entry.encoder = enc.clone();
        }
    }
    Ok(())
}

fn resolve_run_overrides(args: &RunArgs) -> Result<(Option<SinkConfig>, Option<EncoderConfig>)> {
    let sink = if let Some(ref path) = args.output {
        Some(SinkConfig::File {
            path: path.display().to_string(),
        })
    } else if let Some(ref s) = args.sink {
        Some(parse_sink_override(s, args.endpoint.as_deref())?)
    } else {
        None
    };
    let encoder = match args.encoder {
        Some(ref name) => Some(parse_encoder_config(name)?),
        None => None,
    };
    Ok((sink, encoder))
}

fn parse_encoder_config(encoder: &str) -> Result<EncoderConfig> {
    match encoder {
        "prometheus_text" => Ok(EncoderConfig::PrometheusText { precision: None }),
        "influx_lp" => Ok(EncoderConfig::InfluxLineProtocol {
            field_key: None,
            precision: None,
        }),
        "json_lines" => Ok(EncoderConfig::JsonLines { precision: None }),
        "syslog" => Ok(EncoderConfig::Syslog {
            hostname: None,
            app_name: None,
        }),
        "remote_write" => {
            #[cfg(feature = "remote-write")]
            {
                Ok(EncoderConfig::RemoteWrite)
            }
            #[cfg(not(feature = "remote-write"))]
            {
                bail!("--encoder remote_write requires the remote-write feature: cargo build -F remote-write")
            }
        }
        "otlp" => {
            #[cfg(feature = "otlp")]
            {
                Ok(EncoderConfig::Otlp)
            }
            #[cfg(not(feature = "otlp"))]
            {
                bail!("--encoder otlp requires the otlp feature: cargo build -F otlp")
            }
        }
        other => bail!(
            "unknown encoder {:?}: expected one of prometheus_text, influx_lp, json_lines, syslog, remote_write, otlp",
            other
        ),
    }
}

fn parse_sink_override(name: &str, endpoint: Option<&str>) -> Result<SinkConfig> {
    match name {
        "stdout" => Ok(SinkConfig::Stdout),
        "file" => {
            let path = endpoint
                .ok_or_else(|| anyhow!("--sink file requires --endpoint <path>"))?;
            Ok(SinkConfig::File {
                path: path.to_string(),
            })
        }
        "tcp" => {
            let addr = endpoint
                .ok_or_else(|| anyhow!("--sink tcp requires --endpoint <address>"))?;
            Ok(SinkConfig::Tcp {
                address: addr.to_string(),
                retry: None,
            })
        }
        "udp" => {
            let addr = endpoint
                .ok_or_else(|| anyhow!("--sink udp requires --endpoint <address>"))?;
            Ok(SinkConfig::Udp {
                address: addr.to_string(),
            })
        }
        "http_push" => {
            #[cfg(feature = "http")]
            {
                let url = endpoint
                    .ok_or_else(|| anyhow!("--sink http_push requires --endpoint <url>"))?;
                Ok(SinkConfig::HttpPush {
                    url: url.to_string(),
                    content_type: None,
                    batch_size: None,
                    max_buffer_age: None,
                    headers: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = endpoint;
                bail!("--sink http_push requires the http feature: cargo build -F http")
            }
        }
        "loki" => {
            #[cfg(feature = "http")]
            {
                let url = endpoint
                    .ok_or_else(|| anyhow!("--sink loki requires --endpoint <url>"))?;
                Ok(SinkConfig::Loki {
                    url: url.to_string(),
                    batch_size: None,
                    max_buffer_age: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = endpoint;
                bail!("--sink loki requires the http feature: cargo build -F http")
            }
        }
        "remote_write" => {
            #[cfg(feature = "remote-write")]
            {
                let url = endpoint
                    .ok_or_else(|| anyhow!("--sink remote_write requires --endpoint <url>"))?;
                Ok(SinkConfig::RemoteWrite {
                    url: url.to_string(),
                    batch_size: None,
                    max_buffer_age: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "remote-write"))]
            {
                let _ = endpoint;
                bail!("--sink remote_write requires the remote-write feature: cargo build -F remote-write")
            }
        }
        "otlp_grpc" => {
            #[cfg(feature = "otlp")]
            {
                let ep = endpoint
                    .ok_or_else(|| anyhow!("--sink otlp_grpc requires --endpoint <url>"))?;
                Ok(SinkConfig::OtlpGrpc {
                    endpoint: ep.to_string(),
                    signal_type: sonda_core::sink::otlp_grpc::OtlpSignalType::Metrics,
                    batch_size: None,
                    max_buffer_age: None,
                    retry: None,
                })
            }
            #[cfg(not(feature = "otlp"))]
            {
                let _ = endpoint;
                bail!("--sink otlp_grpc requires the otlp feature: cargo build -F otlp")
            }
        }
        other => bail!(
            "unknown sink {:?}; expected one of: stdout, file, tcp, udp, http_push, loki, remote_write, otlp_grpc",
            other
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_reads_file_directly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scenario.yaml");
        fs::write(&path, "version: 2\n").expect("write fixture");
        let yaml = resolve_scenario_source(path.to_str().unwrap(), None).expect("must read file");
        assert!(yaml.contains("version: 2"));
    }

    #[test]
    fn resolve_at_name_without_catalog_returns_error() {
        let err = resolve_scenario_source("@example", None).expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("--catalog"), "got: {msg}");
    }

    #[test]
    fn resolve_at_name_with_unknown_entry_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = resolve_scenario_source("@missing", Some(dir.path())).expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("missing"), "got: {msg}");
    }

    #[test]
    fn resolve_path_not_found_includes_catalog_hint() {
        let err = resolve_scenario_source("/nonexistent/file.yaml", None).expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("--catalog"), "must hint at catalog: {msg}");
    }

    #[test]
    fn parse_sink_stdout() {
        let s = parse_sink_override("stdout", None).expect("stdout parses");
        assert!(matches!(s, SinkConfig::Stdout));
    }

    #[test]
    fn parse_sink_file_requires_endpoint() {
        let err = parse_sink_override("file", None).expect_err("file needs endpoint");
        assert!(format!("{err}").contains("--endpoint"));
    }

    #[test]
    fn parse_encoder_prometheus() {
        let e = parse_encoder_config("prometheus_text").expect("parses");
        assert!(matches!(e, EncoderConfig::PrometheusText { .. }));
    }

    #[test]
    fn parse_encoder_unknown_returns_error() {
        let err = parse_encoder_config("xml").expect_err("must error");
        assert!(format!("{err}").contains("unknown encoder"));
    }
}
