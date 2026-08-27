//! `sonda new --from-prometheus` — capture a live range query as a replayable scenario.
//!
//! Every decision belongs to `sonda_core::acquire`: this module resolves the
//! time window from the flags, builds the auth, and writes the two files core
//! produced. It does not parse PromQL, inspect the shape of a signal, or pick a
//! generator — a capture replays what the database reported.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sonda_core::acquire::normalize::{normalize, Grid};
use sonda_core::acquire::tsdb::{Auth, TsdbClient};
use sonda_core::acquire::{csv_out, yaml_out};

use crate::cli::NewArgs;

/// Past this many series a capture stops being a scenario anyone can read, and
/// the CSV grows a column per series. The number is a judgement, not a limit of
/// the format; the error says how to get under it.
const MAX_SERIES: usize = 20;

/// A bearer token is read from the environment so it never reaches a shell
/// history, a process listing, or a CI log of the command line.
const TOKEN_ENV: &str = "SONDA_PROM_TOKEN";

/// Overall budget for the one range query. Generous, because a wide range at a
/// fine step is slow server-side, but finite so an unreachable endpoint fails.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Fetch the query, write the CSV to `--out`, and return the scenario YAML.
pub fn capture(args: &NewArgs, now_unix: f64) -> Result<String> {
    let url = args
        .from_prometheus
        .as_deref()
        .expect("caller checked --from-prometheus is set");
    let query = args
        .query
        .as_deref()
        .context("--query is required with --from-prometheus")?;
    let csv_path = args.out.as_deref().context(
        "--out is required with --from-prometheus: the emitted scenario \
                  references the CSV by path, so it has to be written somewhere",
    )?;

    let step_text = args
        .step
        .as_deref()
        .context("--step is required with --from-prometheus")?;
    let step = sonda_core::config::validate::parse_duration(step_text)
        .with_context(|| format!("--step {step_text:?} is not a duration"))?;

    let (start, end) = resolve_window(args, now_unix)?;
    let timescale = args.timescale.unwrap_or(1.0);

    let client = TsdbClient::new(url, auth_from(args)?, FETCH_TIMEOUT);
    let series = client
        .fetch_range(query, start, end, step)
        .context("the range query failed")?;

    if series.is_empty() {
        bail!("the query matched no series over that range; widen --range or check --query");
    }
    if series.len() > MAX_SERIES {
        bail!(
            "the query matched {} series, over the cap of {MAX_SERIES}. Aggregate \
             (`sum by (job) (...)`) or add matchers to narrow it.",
            series.len()
        );
    }

    let grid = Grid::new(start, end, step.as_secs_f64()).with_context(|| {
        format!(
            "start {start}, end {end} and step {}s do not describe a grid",
            step.as_secs_f64()
        )
    })?;
    let normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();

    let csv = csv_out::write_csv(grid, &normalized).context("building the capture CSV")?;
    write_file(csv_path, &csv)?;

    let file = yaml_out::scenario_for(&path_for_yaml(csv_path)?, grid, &normalized, timescale)
        .context("building the scenario for the capture")?;
    let yaml = yaml_out::to_yaml(&file).context("rendering the scenario")?;

    let gaps: usize = normalized.iter().map(|s| s.gap_count()).sum();
    eprintln!(
        "captured {} series over {} points to {} ({gaps} missing sample{})",
        normalized.len(),
        grid.len,
        csv_path.display(),
        if gaps == 1 { "" } else { "s" }
    );
    Ok(yaml)
}

/// Resolve `--range` or `--start`/`--end` into a unix-seconds window.
///
/// `--range` is relative to `now_unix`, which the caller reads once so a
/// capture and its log line agree on when "now" was.
fn resolve_window(args: &NewArgs, now_unix: f64) -> Result<(f64, f64)> {
    match (&args.range, &args.start, &args.end) {
        (Some(range), None, None) => {
            let span = sonda_core::config::validate::parse_duration(range)
                .with_context(|| format!("--range {range:?} is not a duration"))?;
            Ok((now_unix - span.as_secs_f64(), now_unix))
        }
        (None, Some(start), Some(end)) => {
            let (s, e) = (parse_instant(start)?, parse_instant(end)?);
            if e <= s {
                bail!("--end ({end}) must be after --start ({start})");
            }
            Ok((s, e))
        }
        (None, Some(_), None) | (None, None, Some(_)) => {
            bail!("--start and --end go together; pass both, or use --range")
        }
        (Some(_), _, _) => bail!("--range and --start/--end are alternatives; pass one"),
        (None, None, None) => {
            bail!("a capture needs a window: pass --range, or --start and --end")
        }
    }
}

/// Parse an instant as unix seconds or as RFC 3339.
fn parse_instant(text: &str) -> Result<f64> {
    if let Ok(secs) = text.parse::<f64>() {
        if secs.is_finite() {
            return Ok(secs);
        }
    }
    let stamp = chrono::DateTime::parse_from_rfc3339(text)
        .with_context(|| format!("{text:?} is neither unix seconds nor an RFC 3339 timestamp"))?;
    Ok(stamp.timestamp() as f64 + f64::from(stamp.timestamp_subsec_millis()) / 1000.0)
}

/// Build the credential from `--header` and the token environment variable.
///
/// Both may be set: the token becomes an `Authorization` header alongside the
/// rest. Neither the value nor the [`Auth`] carrying it is ever printed — see
/// that type's docs.
fn auth_from(args: &NewArgs) -> Result<Auth> {
    let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    if args.headers.is_empty() {
        return Ok(match token {
            Some(t) => Auth::Bearer(t),
            None => Auth::None,
        });
    }
    let mut headers = Vec::with_capacity(args.headers.len() + 1);
    if let Some(t) = token {
        headers.push(("Authorization".to_string(), format!("Bearer {t}")));
    }
    headers.extend(args.headers.iter().cloned());
    Ok(Auth::Headers(headers))
}

/// The CSV path as the emitted scenario should carry it.
fn path_for_yaml(csv_path: &Path) -> Result<String> {
    csv_path
        .to_str()
        .map(str::to_string)
        .with_context(|| format!("--out {} is not valid UTF-8", csv_path.display()))
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dir {}", parent.display()))?;
        }
    }
    std::fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> NewArgs {
        NewArgs {
            template: false,
            from: None,
            output: None,
            from_prometheus: Some("http://localhost:9090".to_string()),
            query: Some("up".to_string()),
            range: None,
            start: None,
            end: None,
            step: Some("15s".to_string()),
            out: None,
            timescale: None,
            headers: Vec::new(),
        }
    }

    #[test]
    fn a_range_is_measured_back_from_the_caller_s_now() {
        let mut a = args();
        a.range = Some("1h".to_string());
        assert_eq!(
            resolve_window(&a, 10_000.0).expect("window"),
            (6_400.0, 10_000.0)
        );
    }

    #[test]
    fn explicit_bounds_accept_unix_seconds_and_rfc3339() {
        let mut a = args();
        a.start = Some("100".to_string());
        a.end = Some("200.5".to_string());
        assert_eq!(resolve_window(&a, 0.0).expect("window"), (100.0, 200.5));

        a.start = Some("1970-01-01T00:01:40Z".to_string());
        a.end = Some("1970-01-01T00:03:20Z".to_string());
        assert_eq!(resolve_window(&a, 0.0).expect("window"), (100.0, 200.0));
    }

    /// One rejected window: the flags as a caller would type them, and the
    /// phrase the error has to carry.
    struct Rejected {
        range: Option<&'static str>,
        start: Option<&'static str>,
        end: Option<&'static str>,
        needle: &'static str,
    }

    #[test]
    fn a_window_that_is_missing_ambiguous_or_inverted_is_refused() {
        let case = |range, start, end, needle| Rejected {
            range,
            start,
            end,
            needle,
        };
        let cases = [
            case(None, None, None, "needs a window"),
            case(None, Some("100"), None, "go together"),
            case(None, None, Some("200"), "go together"),
            case(Some("1h"), Some("100"), Some("200"), "alternatives"),
            case(None, Some("200"), Some("100"), "must be after"),
        ];
        for c in cases {
            let mut a = args();
            a.range = c.range.map(str::to_string);
            a.start = c.start.map(str::to_string);
            a.end = c.end.map(str::to_string);
            let err = resolve_window(&a, 0.0).expect_err(&format!(
                "{:?}/{:?}/{:?} must be refused",
                c.range, c.start, c.end
            ));
            assert!(
                err.to_string().contains(c.needle),
                "expected {:?} in: {err}",
                c.needle
            );
        }
    }

    #[test]
    fn an_unparseable_instant_says_both_forms_it_accepts() {
        let mut a = args();
        a.start = Some("yesterday".to_string());
        a.end = Some("200".to_string());
        let err = resolve_window(&a, 0.0).expect_err("must be refused");
        let text = format!("{err:#}");
        assert!(text.contains("unix seconds"), "{text}");
        assert!(text.contains("RFC 3339"), "{text}");
    }
}
