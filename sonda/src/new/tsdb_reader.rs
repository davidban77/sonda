//! `sonda new --from-prometheus` — capture a live range query as a replayable scenario.
//!
//! Every decision belongs to `sonda_core::acquire`: this module resolves the
//! time window from the flags, builds the auth, and writes the two files core
//! produced. It does not parse PromQL, inspect the shape of a signal, or pick a
//! generator — a capture replays what the database reported.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sonda_core::acquire::normalize::{normalize, Grid, NormalizedSeries};
use sonda_core::acquire::tsdb::{Auth, TsdbClient};
use sonda_core::acquire::{csv_out, yaml_out};
use sonda_core::SondaError;

use crate::cli::NewArgs;

/// Flatten a [`SondaError`] into one message.
///
/// Its `Display` already contains the source's text *and* `#[from]` exposes
/// that source, so anyhow's `{:#}` walks the chain and prints the same sentence
/// twice. Flattening here keeps every error this path reports readable; the
/// duplication itself is core's and predates this module.
fn flat(e: SondaError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

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
        .map_err(flat)
        .with_context(|| format!("--step {step_text:?} is not a duration"))?;

    // Everything that can be rejected from the flags alone is rejected here,
    // before the network call and before anything is written. `scenario_for`
    // validates the timescale too, but that runs after the CSV has landed —
    // which would leave a capture on disk with no scenario beside it.
    let (start, end) = resolve_window(args, now_unix)?;
    let timescale = resolve_timescale(args)?;
    if let Some(name) = args.metric_name.as_deref() {
        sonda_core::ValidatedMetricName::new(name)
            .map_err(flat)
            .context("--metric-name")?;
    }

    let client = TsdbClient::new(url, auth_from(args), FETCH_TIMEOUT);
    let series = client
        .fetch_range(query, start, end, step)
        .map_err(flat)
        .context("the range query failed")?;

    if series.is_empty() {
        bail!("the query matched no series over that range; widen --range or check --query");
    }
    if series.len() > MAX_SERIES {
        bail!(
            "the query matched {} series, over the cap of {MAX_SERIES}. Add matchers to narrow \
             it, or aggregate — `sum by (job) (...)` drops the metric name, so an aggregated \
             query needs --metric-name <NAME> to supply one.",
            series.len()
        );
    }

    let grid = Grid::new(start, end, step.as_secs_f64()).with_context(|| {
        format!(
            "start {start}, end {end} and step {}s do not describe a grid",
            step.as_secs_f64()
        )
    })?;
    let mut normalized: Vec<_> = series.iter().map(|s| normalize(s, grid)).collect();
    name_unnamed_series(&mut normalized, args.metric_name.as_deref())?;

    let csv = csv_out::write_csv(grid, &normalized)
        .map_err(flat)
        .context("building the capture CSV")?;
    write_file(csv_path, &csv)?;

    let file = yaml_out::scenario_for(&path_for_yaml(csv_path)?, grid, &normalized, timescale)
        .map_err(flat)
        .context("building the scenario for the capture")?;
    let yaml = yaml_out::to_yaml(&file)
        .map_err(flat)
        .context("rendering the scenario")?;

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

/// The replay speed, refused here rather than deep in the emitter.
fn resolve_timescale(args: &NewArgs) -> Result<f64> {
    let t = args.timescale.unwrap_or(1.0);
    if !t.is_finite() || t <= 0.0 {
        bail!("--timescale must be a positive finite number, got {t}");
    }
    Ok(t)
}

/// Give `--metric-name` to every series the query left without one.
///
/// PromQL aggregations drop `__name__`, and `sum by (job) (...)` is the usual
/// way to get under the series cap — so the two would otherwise contradict each
/// other: take the cap's advice and the capture is refused for having no name.
fn name_unnamed_series(series: &mut [NormalizedSeries], name: Option<&str>) -> Result<()> {
    let missing = series
        .iter()
        .filter(|s| !s.labels.contains_key("__name__"))
        .count();
    if missing == 0 {
        return Ok(());
    }
    let Some(name) = name else {
        bail!(
            "{missing} of {} series carry no metric name. PromQL aggregations such as \
             `sum by (job) (...)` drop __name__; pass --metric-name <NAME> to supply one.",
            series.len()
        );
    };
    for s in series.iter_mut() {
        s.labels
            .entry("__name__".to_string())
            .or_insert_with(|| name.to_string());
    }
    Ok(())
}

/// Resolve `--range` or `--start`/`--end` into a unix-seconds window.
///
/// `--range` is relative to `now_unix`, which the caller reads once so a
/// capture and its log line agree on when "now" was.
fn resolve_window(args: &NewArgs, now_unix: f64) -> Result<(f64, f64)> {
    match (&args.range, &args.start, &args.end) {
        (Some(range), None, None) => {
            let span = sonda_core::config::validate::parse_duration(range)
                .map_err(flat)
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
/// Both may be set and the token joins the headers — except when one of them is
/// itself an `Authorization`, where the explicit flag wins and the token is
/// dropped with a note. Silently sending one of two credentials is the part
/// worth avoiding. Neither the value nor the [`Auth`] carrying it is ever
/// printed — see that type's docs.
fn auth_from(args: &NewArgs) -> Auth {
    let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    if args.headers.is_empty() {
        return match token {
            Some(t) => Auth::Bearer(t),
            None => Auth::None,
        };
    }

    let explicit_auth = args
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Authorization"));
    let mut headers = Vec::with_capacity(args.headers.len() + 1);
    match token {
        Some(_) if explicit_auth => eprintln!(
            "note: --header Authorization overrides {TOKEN_ENV}; the environment token is unused"
        ),
        Some(t) => headers.push(("Authorization".to_string(), format!("Bearer {t}"))),
        None => {}
    }
    headers.extend(args.headers.iter().cloned());
    Auth::Headers(headers)
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
            metric_name: None,
        }
    }

    fn series(name: Option<&str>) -> NormalizedSeries {
        let mut labels = std::collections::BTreeMap::new();
        if let Some(n) = name {
            labels.insert("__name__".to_string(), n.to_string());
        }
        labels.insert("job".to_string(), "api".to_string());
        NormalizedSeries {
            labels,
            values: vec![Some(1.0)],
        }
    }

    #[test]
    fn a_metric_name_fills_in_only_the_series_that_lack_one() {
        let mut s = [series(None), series(Some("kept"))];
        name_unnamed_series(&mut s, Some("supplied")).expect("named");
        assert_eq!(
            s[0].labels.get("__name__").map(String::as_str),
            Some("supplied")
        );
        assert_eq!(
            s[1].labels.get("__name__").map(String::as_str),
            Some("kept"),
            "an existing name is not overwritten"
        );
    }

    #[test]
    fn a_nameless_series_without_the_flag_says_which_flag_supplies_one() {
        let mut s = [series(None), series(Some("kept"))];
        let err = name_unnamed_series(&mut s, None).expect_err("must refuse");
        let text = err.to_string();
        assert!(text.contains("1 of 2"), "it counts them: {text}");
        assert!(text.contains("--metric-name"), "and names the flag: {text}");
        assert!(text.contains("sum by"), "and why it happened: {text}");
    }

    #[test]
    fn a_fully_named_result_needs_no_flag() {
        let mut s = [series(Some("a")), series(Some("b"))];
        name_unnamed_series(&mut s, None).expect("nothing to fill in");
    }

    #[test]
    fn a_timescale_that_is_not_positive_is_refused_before_anything_is_written() {
        for bad in ["0", "-1", "nan", "inf"] {
            let mut a = args();
            a.timescale = Some(bad.parse().expect("f64"));
            let err =
                resolve_timescale(&a).expect_err(&format!("--timescale {bad} must be refused"));
            assert!(err.to_string().contains("--timescale"), "{err}");
        }
        let mut a = args();
        a.timescale = Some(4.0);
        assert_eq!(resolve_timescale(&a).expect("valid"), 4.0);
        assert_eq!(resolve_timescale(&args()).expect("default"), 1.0);
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
