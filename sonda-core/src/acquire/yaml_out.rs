//! Emit the scenario file that replays a capture.
//!
//! The companion to [`super::csv_out`]. That module writes the data — values,
//! blanks, one row per grid point — and this one writes the scenario that
//! plays it back at the cadence it was captured at, with the recorded silence
//! declared as `gap_windows:`.
//!
//! Feature-gated on `config`: it builds the compiler's own [`ScenarioFile`] and
//! serialises that, so the emitted grammar is whatever serde produces for the
//! types the compiler already reads. Nothing writes YAML by hand.
//!
//! Two rules the emitted scenarios keep:
//!
//! * **One block per distinct absence pattern.** `gap_windows:` is
//!   scenario-level, so a block speaks for every column it lists; columns whose
//!   silence falls on different rows cannot share one.
//! * **`repeat: false` on every block**, silence or not. The generator defaults
//!   to `true`, so omitting it would loop a dense capture — and for a capture
//!   with silence the engine refuses the combination outright, since
//!   `gap_windows:` describe a single pass.

use super::normalize::{Grid, NormalizedSeries};
use crate::compiler::{Entry, Kind, ScenarioFile};
use crate::config::GapWindowConfig;
use crate::generator::{CsvColumnSpec, GeneratorConfig};
use crate::{ConfigError, SondaError};

/// Build the scenario file that replays `series` from `csv_path`.
///
/// `csv_path` is written into the emitted `file:` field verbatim; the caller
/// decides whether it is absolute or relative to the scenario, and nothing here
/// touches the filesystem.
///
/// Column indices are 1-based in the emitted spec because column 0 of the file
/// [`super::csv_out::write_csv`] wrote is the timestamp — series `i` is CSV
/// column `i + 1`. Getting that off by one would silently replay the timestamps
/// as values.
///
/// # Errors
///
/// Returns [`SondaError::Config`] when the grid cannot be expressed as a rate,
/// when a series carries no metric name to build a scenario name from, or when
/// [`super::csv_out::gap_windows_for`] cannot express a column's silence.
pub fn scenario_for(
    csv_path: &str,
    grid: Grid,
    series: &[NormalizedSeries],
) -> Result<ScenarioFile, SondaError> {
    if series.is_empty() {
        return Err(SondaError::Config(ConfigError::invalid(
            "acquire: cannot build a scenario from zero series; the query matched nothing",
        )));
    }
    if grid.step <= 0.0 || !grid.step.is_finite() {
        return Err(SondaError::Config(ConfigError::invalid(format!(
            "acquire: grid step {} cannot be expressed as a replay rate",
            grid.step
        ))));
    }

    // The engine's step, not the caller's — see `file_step`. Windows, rate and
    // duration all come from this one value.
    let step = file_step(grid)?;
    let rate = 1.0 / step;
    let duration = format!("{}s", grid.len as f64 * step);

    let mut blocks: Vec<(Vec<usize>, Vec<GapWindowConfig>)> = Vec::new();
    for (i, s) in series.iter().enumerate() {
        let windows = super::csv_out::gap_windows_for(&s.values, step)?;
        // Group by the windows themselves rather than by the absent-row set:
        // two columns belong together exactly when the same `gap_windows:`
        // block is correct for both, and that is what the windows are.
        let key = |w: &[GapWindowConfig]| -> Vec<(String, String)> {
            w.iter().map(|g| (g.at.clone(), g.r#for.clone())).collect()
        };
        let this = key(&windows);
        match blocks.iter_mut().find(|(_, w)| key(w) == this) {
            Some((members, _)) => members.push(i),
            None => blocks.push((vec![i], windows)),
        }
    }

    let mut scenarios = Vec::with_capacity(blocks.len());
    for (block_index, (members, windows)) in blocks.into_iter().enumerate() {
        let mut columns = Vec::with_capacity(members.len());
        for &i in &members {
            let name = series[i].labels.get("__name__").ok_or_else(|| {
                SondaError::Config(ConfigError::invalid(format!(
                    "acquire: series {i} has no `__name__` label, so the emitted scenario has \
                     no metric name for it. Query without an aggregation that drops the name, \
                     or set one on the column before emitting."
                )))
            })?;
            columns.push(CsvColumnSpec {
                index: i + 1,
                name: name.clone(),
                labels: Some(
                    series[i]
                        .labels
                        .iter()
                        .filter(|(k, _)| k.as_str() != "__name__")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            });
        }

        // Every field listed, no `..`: a field added to `Entry` later should
        // force a decision here rather than defaulting into an emitted
        // scenario. `jitter` must stay `None` — it perturbs the value a replay
        // exists to reproduce exactly.
        scenarios.push(Entry {
            id: Some(format!("capture_{block_index}")),
            signal_type: "metrics".to_string(),
            name: Some(format!("capture_{block_index}")),
            rate: Some(rate),
            duration: Some(duration.clone()),
            start_time: None,
            generator: Some(GeneratorConfig::CsvReplay {
                file: csv_path.to_string(),
                column: None,
                columns: Some(columns),
                // Explicit, and never true — see the module docs.
                repeat: Some(false),
                timescale: None,
                default_metric_name: None,
            }),
            log_generator: None,
            labels: None,
            dynamic_labels: None,
            encoder: None,
            sink: None,
            jitter: None,
            jitter_seed: None,
            gaps: None,
            gap_windows: if windows.is_empty() {
                None
            } else {
                Some(windows)
            },
            bursts: None,
            cardinality_spikes: None,
            phase_offset: None,
            clock_group: None,
            after: None,
            while_clause: None,
            delay_clause: None,
            pack: None,
            overrides: None,
            distribution: None,
            buckets: None,
            quantiles: None,
            observations_per_tick: None,
            mean_shift_per_sec: None,
            seed: None,
            on_sink_error: None,
            metric_type: None,
            help: None,
        });
    }

    Ok(ScenarioFile {
        version: 2,
        kind: Kind::Runnable,
        tags: Vec::new(),
        scenario_name: None,
        category: None,
        description: None,
        defaults: None,
        scenarios,
        expect: None,
    })
}

/// The step the engine will replay at, given the file [`super::csv_out::write_csv`]
/// will write.
///
/// Not `grid.step`: the CSV carries millisecond timestamps, so `1/7` reaches the
/// engine as `0.143`, and windows built from the caller's step drift against the
/// replay until the capture stops loading.
///
/// Both halves are borrowed rather than modelled —
/// [`super::csv_out::format_timestamp`] renders the instants the file will
/// carry, and [`crate::config::median_delta_seconds`] reduces them over
/// [`crate::config::CSV_DELTA_SAMPLE_ROWS`] rows, which is exactly what
/// `compute_csv_delta_seconds` does on the way back in. Any model of that
/// reduction disagrees with it somewhere.
fn file_step(grid: Grid) -> Result<f64, SondaError> {
    // A single-row capture has no delta to read, so nothing rounds it and the
    // caller's step stands.
    if grid.len < 2 {
        return Ok(grid.step);
    }
    let sampled = grid.len.min(crate::config::CSV_DELTA_SAMPLE_ROWS);
    let mut instants = Vec::with_capacity(sampled);
    for n in 0..sampled {
        let text = super::csv_out::format_timestamp(grid.point(n));
        instants.push(text.parse::<f64>().map_err(|e| {
            SondaError::Config(ConfigError::invalid(format!(
                "acquire: timestamp {text:?} for grid point {n} is not a number the replay \
                 could read back ({e})"
            )))
        })?);
    }

    crate::config::median_delta_seconds(&instants).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "acquire: a grid step of {}s does not survive the millisecond precision the capture \
             file uses: {e}. Capture at a coarser step.",
            grid.step
        )))
    })
}

/// Render [`scenario_for`]'s output as YAML text.
///
/// # Errors
///
/// Returns [`SondaError::Config`] if serialisation fails, which would mean a
/// config type stopped being representable in YAML.
pub fn to_yaml(file: &ScenarioFile) -> Result<String, SondaError> {
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::to_value(file).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "acquire: emitted scenario could not be serialised: {e}"
        )))
    })?;
    strip_nulls(&mut value);
    serde_yaml_ng::to_string(&value).map_err(|e| {
        SondaError::Config(ConfigError::invalid(format!(
            "acquire: emitted scenario could not be rendered: {e}"
        )))
    })
}

/// Drop every `null`-valued key, recursively.
///
/// `Entry` has 34 fields and an emitted scenario sets a handful, so serialising
/// straight renders thirty `field: null` lines per block. An omitted key and a
/// null one mean the same thing to the parser, and `jitter: null` in a file
/// meant for exact replay reads like a setting to anyone grepping it.
fn strip_nulls(value: &mut serde_yaml_ng::Value) {
    match value {
        serde_yaml_ng::Value::Mapping(map) => {
            map.retain(|_, v| !v.is_null());
            for (_, v) in map.iter_mut() {
                strip_nulls(v);
            }
        }
        serde_yaml_ng::Value::Sequence(seq) => {
            for v in seq.iter_mut() {
                strip_nulls(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquire::csv_out::write_csv;
    use crate::compiler::expand::InMemoryPackResolver;
    use std::collections::BTreeMap;

    fn norm(name: &str, extra: &[(&str, &str)], values: &[Option<f64>]) -> NormalizedSeries {
        let mut labels = BTreeMap::new();
        labels.insert("__name__".to_string(), name.to_string());
        for (k, v) in extra {
            labels.insert((*k).to_string(), (*v).to_string());
        }
        NormalizedSeries {
            labels,
            values: values.to_vec(),
        }
    }

    /// Write both halves to a temp dir and push the scenario through the real
    /// compiler, which is the entry point `sonda run` uses. A scenario that
    /// only *looks* right fails here.
    /// Returns the temp dir alongside the entries: `expand_entry` reads the
    /// CSV to resolve columns, so the file has to outlive the compile.
    fn compile_emitted(
        grid: Grid,
        series: &[NormalizedSeries],
    ) -> (tempfile::TempDir, Vec<crate::config::ScenarioEntry>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let csv_path = dir.path().join("capture.csv");
        std::fs::write(&csv_path, write_csv(grid, series).expect("csv")).expect("write csv");

        let file = scenario_for(csv_path.to_str().expect("utf8 path"), grid, series)
            .expect("scenario must build");
        let yaml = to_yaml(&file).expect("yaml");
        let entries = crate::compile_scenario_file(&yaml, &InMemoryPackResolver::default())
            .unwrap_or_else(|e| {
                panic!("the real compiler rejected the emitted scenario: {e}\n{yaml}")
            });
        (dir, entries)
    }

    #[test]
    fn the_emitted_scenario_compiles_and_loads_its_own_capture() {
        let grid = Grid::new(0.0, 4.0, 1.0).expect("grid");
        let series = [norm(
            "m",
            &[("job", "api")],
            &[Some(1.0), None, Some(3.0), Some(4.0), Some(5.0)],
        )];
        let (_dir, entries) = compile_emitted(grid, &series);
        assert_eq!(entries.len(), 1, "one column, one scenario");
    }

    #[test]
    fn columns_sharing_an_absence_pattern_share_a_block_and_others_do_not() {
        let grid = Grid::new(0.0, 3.0, 1.0).expect("grid");
        // a and b go silent on the same row; c on a different one.
        let series = [
            norm("a", &[], &[Some(1.0), None, Some(3.0), Some(4.0)]),
            norm("b", &[], &[Some(9.0), None, Some(7.0), Some(6.0)]),
            norm("c", &[], &[Some(1.0), Some(2.0), None, Some(4.0)]),
        ];
        let file = scenario_for("capture.csv", grid, &series).expect("scenario");
        assert_eq!(
            file.scenarios.len(),
            2,
            "two distinct absence patterns, two blocks: {:#?}",
            file.scenarios
        );

        // The compiler accepts the whole thing: one entry per block, because
        // the column fan-out happens later, at `expand_entry` — the same call
        // `prepare_entries` makes on the way to launch.
        let (_dir, entries) = compile_emitted(grid, &series);
        assert_eq!(entries.len(), 2, "compile yields one entry per block");

        let runnables: usize = entries
            .iter()
            .map(|e| {
                crate::config::expand_entry(e.clone())
                    .expect("the emitted columns must expand")
                    .len()
            })
            .sum();
        assert_eq!(
            runnables, 3,
            "and the three columns fan out to three runnables at launch"
        );
    }

    #[test]
    fn every_block_sets_repeat_false_even_with_no_silence_to_declare() {
        let grid = Grid::new(0.0, 2.0, 1.0).expect("grid");
        let series = [norm("dense", &[], &[Some(1.0), Some(2.0), Some(3.0)])];
        let file = scenario_for("capture.csv", grid, &series).expect("scenario");
        let entry = &file.scenarios[0];
        assert!(
            entry.gap_windows.is_none(),
            "no silence, so nothing to declare"
        );
        match entry.generator.as_ref().expect("generator") {
            GeneratorConfig::CsvReplay { repeat, .. } => assert_eq!(
                *repeat,
                Some(false),
                "repeat is written explicitly; the generator default is true and would loop"
            ),
            other => panic!("expected csv_replay, got {other:?}"),
        }
    }

    #[test]
    fn nothing_emitted_carries_jitter() {
        // WP19 asserts this over the whole file; here it is asserted at the
        // source, because jitter perturbs the value and a replay exists to
        // reproduce it exactly.
        let grid = Grid::new(0.0, 2.0, 1.0).expect("grid");
        let series = [norm("m", &[], &[Some(1.0), None, Some(3.0)])];
        let file = scenario_for("capture.csv", grid, &series).expect("scenario");
        for e in &file.scenarios {
            assert!(e.jitter.is_none() && e.jitter_seed.is_none(), "no jitter");
        }
        assert!(
            !to_yaml(&file).expect("yaml").contains("jitter"),
            "and none in the rendered text either"
        );
    }

    #[test]
    fn columns_are_offset_past_the_timestamp_column() {
        let grid = Grid::new(0.0, 1.0, 1.0).expect("grid");
        let series = [
            norm("first", &[], &[Some(1.0), Some(2.0)]),
            norm("second", &[], &[Some(3.0), Some(4.0)]),
        ];
        let file = scenario_for("capture.csv", grid, &series).expect("scenario");
        match file.scenarios[0].generator.as_ref().expect("generator") {
            GeneratorConfig::CsvReplay { columns, .. } => {
                let idx: Vec<usize> = columns
                    .as_ref()
                    .expect("columns")
                    .iter()
                    .map(|c| c.index)
                    .collect();
                assert_eq!(
                    idx,
                    vec![1, 2],
                    "series i is CSV column i+1; 0 is timestamp"
                );
            }
            other => panic!("expected csv_replay, got {other:?}"),
        }
    }

    #[test]
    fn a_step_that_is_not_a_whole_millisecond_still_round_trips_at_length() {
        // The capture file carries millisecond timestamps and `csv_replay`
        // takes its interval from them, so a step like 1/7 reaches the engine
        // rounded. Windows built from the caller's exact step drift against
        // that by a fixed amount per row, and the error accumulates until a
        // window misses the row it was built for.
        //
        // Short captures at round steps do not reach it — every other test in
        // this file passes under the drifting version — so the length and the
        // steps here are the point. Measured against the old behaviour: 1/7
        // and 1/3 failed from roughly 600 rows, 0.123456789 from roughly 200.
        const ROWS: usize = 1400;
        // The last three are the ones a first-delta model gets wrong: a step
        // whose millisecond value is a clean half rounds one way as a single
        // delta and the other way as a median, so the emitter and the engine
        // land on different numbers. 0.0015 emitted 0.002 against the engine's
        // 0.0015 — 0.199s of drift by row 398, against a 0.001s margin.
        for step in [1.0 / 7.0, 1.0 / 3.0, 0.123_456_789, 0.0015, 0.0025, 0.0035] {
            // Absences at high row indices, where linear drift is largest.
            let absent = [3usize, ROWS - 400, ROWS - 399, ROWS - 2];
            let values: Vec<Option<f64>> = (0..ROWS)
                .map(|i| {
                    if absent.contains(&i) {
                        None
                    } else {
                        Some(i as f64)
                    }
                })
                .collect();
            let grid = Grid::new(0.0, step * (ROWS - 1) as f64, step).expect("grid");
            let series = [norm("m", &[], &values)];
            let (_dir, entries) = compile_emitted(grid, &series);
            for e in entries {
                crate::config::expand_entry(e).unwrap_or_else(|err| {
                    panic!("step {step} at {ROWS} rows: the engine refused the pair: {err}")
                });
            }
        }
    }

    #[test]
    fn a_series_with_no_metric_name_is_refused_with_the_reason() {
        let grid = Grid::new(0.0, 1.0, 1.0).expect("grid");
        let nameless = NormalizedSeries {
            labels: BTreeMap::new(),
            values: vec![Some(1.0), Some(2.0)],
        };
        let err = scenario_for("capture.csv", grid, &[nameless]).expect_err("must refuse");
        assert!(
            err.to_string().contains("__name__"),
            "the error names the missing label: {err}"
        );
    }

    #[test]
    fn zero_series_is_refused_rather_than_emitting_an_empty_scenario() {
        let grid = Grid::new(0.0, 1.0, 1.0).expect("grid");
        let err = scenario_for("capture.csv", grid, &[]).expect_err("must refuse");
        assert!(err.to_string().contains("matched nothing"), "{err}");
    }
}
