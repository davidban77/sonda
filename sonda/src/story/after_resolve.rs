//! `after` clause parsing, dependency graph construction, topological sort,
//! and offset computation.
//!
//! An `after` clause like `"interface_oper_state < 1"` is parsed into an
//! [`AfterClause`] struct, then resolved against the signal definitions in
//! the story to compute a concrete `phase_offset` in seconds. Signals are
//! processed in topological order so that transitive dependencies accumulate
//! correctly.

use std::collections::HashMap;

use super::timing::{
    self, flap_crossing_secs, sawtooth_crossing_secs, spike_crossing_secs, Operator, TimingError,
};

/// A parsed `after` clause from a story signal.
///
/// Represents `"<metric_ref> <operator> <threshold>"`.
#[derive(Debug, Clone, PartialEq)]
pub struct AfterClause {
    /// The metric name this clause references.
    pub metric_ref: String,
    /// The comparison operator.
    pub operator: Operator,
    /// The numeric threshold value.
    pub threshold: f64,
}

/// Parse an `after` clause string into an [`AfterClause`].
///
/// Expected format: `"metric_name < 1"` or `"metric_name > 70"`.
/// Only `<` and `>` operators are supported.
///
/// # Errors
///
/// Returns an error string if the clause cannot be parsed.
pub fn parse_after_clause(s: &str) -> Result<AfterClause, String> {
    let s = s.trim();

    // Find the operator position. We scan for standalone `<` or `>`.
    let (op_pos, op) = find_operator(s)?;

    let metric_ref = s[..op_pos].trim().to_string();
    if metric_ref.is_empty() {
        return Err("after clause has no metric name before the operator".to_string());
    }

    let threshold_str = s[op_pos + 1..].trim();
    if threshold_str.is_empty() {
        return Err("after clause has no threshold value after the operator".to_string());
    }

    let threshold: f64 = threshold_str
        .parse()
        .map_err(|_| format!("invalid threshold value {threshold_str:?} in after clause"))?;

    Ok(AfterClause {
        metric_ref,
        operator: op,
        threshold,
    })
}

/// Find the operator (`<` or `>`) in an after clause string.
///
/// Returns `(position, Operator)`. Rejects ambiguous or missing operators.
fn find_operator(s: &str) -> Result<(usize, Operator), String> {
    let mut found: Option<(usize, Operator)> = None;

    for (i, ch) in s.char_indices() {
        let op = match ch {
            '<' => Some(Operator::LessThan),
            '>' => Some(Operator::GreaterThan),
            _ => None,
        };
        if let Some(op) = op {
            if found.is_some() {
                return Err(format!(
                    "after clause {s:?} contains multiple operators; \
                     only a single '<' or '>' is allowed"
                ));
            }
            found = Some((i, op));
        }
    }

    found.ok_or_else(|| {
        format!(
            "after clause {s:?} has no operator; \
             expected format: \"metric_name < threshold\" or \"metric_name > threshold\""
        )
    })
}

/// Parameters extracted from a signal definition, needed for timing computation.
#[derive(Debug, Clone)]
pub struct SignalParams {
    /// The behavior alias (e.g., "flap", "saturation", "leak", "degradation",
    /// "spike_event", "steady").
    pub behavior: String,
    /// Flat key-value parameters from the signal definition.
    pub params: HashMap<String, serde_yaml_ng::Value>,
}

/// Resolve all `after` clauses in a story into concrete offsets (in seconds).
///
/// Takes a list of `(metric_name, optional_after_clause, signal_params)` tuples
/// and returns a map from metric name to resolved phase offset in seconds.
///
/// Signals without an `after` clause get offset 0.0. Signals with an `after`
/// clause get the referenced signal's offset plus the timing computation result.
///
/// # Errors
///
/// Returns a descriptive error string for:
/// - Unknown metric references
/// - Circular dependencies
/// - Unsupported behavior aliases (e.g., "steady")
/// - Out-of-range thresholds
pub fn resolve_offsets(
    signals: &[(String, Option<AfterClause>, SignalParams)],
) -> Result<HashMap<String, f64>, String> {
    let name_to_idx: HashMap<&str, usize> = signals
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| (name.as_str(), i))
        .collect();

    // Validate all metric references exist.
    for (name, after, _) in signals {
        if let Some(clause) = after {
            if !name_to_idx.contains_key(clause.metric_ref.as_str()) {
                return Err(format!(
                    "signal {:?}: after clause references {:?} which is not defined in this story",
                    name, clause.metric_ref
                ));
            }
        }
    }

    // Build adjacency list: edges go from dependency -> dependent.
    // in_degree[i] = number of signals that signal i depends on (0 or 1).
    let n = signals.len();
    let mut in_degree = vec![0u32; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, (_, after, _)) in signals.iter().enumerate() {
        if let Some(clause) = after {
            let dep_idx = name_to_idx[clause.metric_ref.as_str()];
            in_degree[i] = 1;
            dependents[dep_idx].push(i);
        }
    }

    // Kahn's algorithm for topological sort.
    let mut queue: Vec<usize> = Vec::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push(i);
        }
    }

    let mut sorted: Vec<usize> = Vec::with_capacity(n);
    let mut offsets = vec![0.0_f64; n];

    while let Some(idx) = queue.pop() {
        sorted.push(idx);

        for &dep_idx in &dependents[idx] {
            in_degree[dep_idx] -= 1;
            if in_degree[dep_idx] == 0 {
                queue.push(dep_idx);
            }
        }
    }

    if sorted.len() < n {
        // Cycle detected — find the cycle for error reporting.
        let cycle = find_cycle(signals, &name_to_idx);
        return Err(format!("circular dependency: {cycle}"));
    }

    // Process in topological order.
    for &idx in &sorted {
        let (name, after, params) = &signals[idx];
        if let Some(clause) = after {
            let dep_idx = name_to_idx[clause.metric_ref.as_str()];
            let dep_offset = offsets[dep_idx];
            let dep_params = &signals[dep_idx].2;

            let crossing_secs = compute_crossing(name, clause, dep_params).map_err(|e| {
                format!(
                    "signal {:?}: after {:?} {} {}: {}",
                    name, clause.metric_ref, clause.operator, clause.threshold, e
                )
            })?;

            offsets[idx] = dep_offset + crossing_secs;
        }
        // else: offset remains 0.0
        let _ = params; // suppress unused warning
    }

    let mut result = HashMap::with_capacity(n);
    for (i, (name, _, _)) in signals.iter().enumerate() {
        result.insert(name.clone(), offsets[i]);
    }

    Ok(result)
}

/// Compute the crossing time for a single `after` clause against a signal's
/// parameters.
fn compute_crossing(
    _signal_name: &str,
    clause: &AfterClause,
    dep_params: &SignalParams,
) -> Result<f64, TimingError> {
    match dep_params.behavior.as_str() {
        "flap" => {
            let up_duration_secs = get_duration_param(&dep_params.params, "up_duration", 10.0)?;
            let down_duration_secs = get_duration_param(&dep_params.params, "down_duration", 5.0)?;
            let up_value = get_f64_param(&dep_params.params, "up_value", 1.0);
            let down_value = get_f64_param(&dep_params.params, "down_value", 0.0);

            flap_crossing_secs(
                clause.operator,
                clause.threshold,
                up_duration_secs,
                down_duration_secs,
                up_value,
                down_value,
            )
        }
        "saturation" => {
            let baseline = get_f64_param(&dep_params.params, "baseline", 0.0);
            let ceiling = get_f64_param(&dep_params.params, "ceiling", 100.0);
            let period_secs = get_duration_param(&dep_params.params, "time_to_saturate", 300.0)?;

            sawtooth_crossing_secs(
                clause.operator,
                clause.threshold,
                baseline,
                ceiling,
                period_secs,
            )
        }
        "leak" => {
            let baseline = get_f64_param(&dep_params.params, "baseline", 0.0);
            let ceiling = get_f64_param(&dep_params.params, "ceiling", 100.0);
            let period_secs = get_duration_param(&dep_params.params, "time_to_ceiling", 600.0)?;

            sawtooth_crossing_secs(
                clause.operator,
                clause.threshold,
                baseline,
                ceiling,
                period_secs,
            )
        }
        "degradation" => {
            let baseline = get_f64_param(&dep_params.params, "baseline", 0.0);
            let ceiling = get_f64_param(&dep_params.params, "ceiling", 100.0);
            let period_secs = get_duration_param(&dep_params.params, "time_to_degrade", 300.0)?;

            sawtooth_crossing_secs(
                clause.operator,
                clause.threshold,
                baseline,
                ceiling,
                period_secs,
            )
        }
        "spike_event" => {
            let baseline = get_f64_param(&dep_params.params, "baseline", 0.0);
            let spike_height = get_f64_param(&dep_params.params, "spike_height", 100.0);
            let spike_duration_secs =
                get_duration_param(&dep_params.params, "spike_duration", 10.0)?;

            spike_crossing_secs(
                clause.operator,
                clause.threshold,
                baseline,
                spike_height,
                spike_duration_secs,
            )
        }
        "steady" => timing::steady_crossing_secs(),
        other => Err(TimingError::Unsupported {
            message: format!(
                "behavior {:?} does not support after-clause threshold crossing computation",
                other
            ),
        }),
    }
}

/// Extract a duration parameter from the signal params, parsing the duration
/// string to seconds. Returns `default_secs` if the key is absent.
fn get_duration_param(
    params: &HashMap<String, serde_yaml_ng::Value>,
    key: &str,
    default_secs: f64,
) -> Result<f64, TimingError> {
    match params.get(key) {
        Some(serde_yaml_ng::Value::String(s)) => {
            let dur = sonda_core::config::validate::parse_duration(s).map_err(|e| {
                TimingError::OutOfRange {
                    message: format!("invalid duration {:?} for {key}: {e}", s),
                }
            })?;
            Ok(dur.as_secs_f64())
        }
        Some(serde_yaml_ng::Value::Number(n)) => {
            // Treat bare numbers as seconds.
            n.as_f64().ok_or_else(|| TimingError::OutOfRange {
                message: format!("{key}: numeric value is not a valid f64"),
            })
        }
        _ => Ok(default_secs),
    }
}

/// Extract an f64 parameter from the signal params. Returns `default` if absent.
fn get_f64_param(params: &HashMap<String, serde_yaml_ng::Value>, key: &str, default: f64) -> f64 {
    match params.get(key) {
        Some(serde_yaml_ng::Value::Number(n)) => n.as_f64().unwrap_or(default),
        _ => default,
    }
}

/// Find a cycle in the dependency graph for error reporting.
///
/// Returns a string like `"A -> B -> C -> A"`.
fn find_cycle(
    signals: &[(String, Option<AfterClause>, SignalParams)],
    name_to_idx: &HashMap<&str, usize>,
) -> String {
    let n = signals.len();
    let mut visited = vec![0u8; n]; // 0=white, 1=gray, 2=black
    let mut parent = vec![usize::MAX; n];

    for start in 0..n {
        if visited[start] != 0 {
            continue;
        }
        // DFS from `start`.
        let mut stack = vec![(start, false)];
        while let Some((node, returning)) = stack.pop() {
            if returning {
                visited[node] = 2;
                continue;
            }
            if visited[node] == 1 {
                // Found a back-edge to a gray node while revisiting — skip.
                continue;
            }
            visited[node] = 1;
            stack.push((node, true)); // return marker

            if let Some(ref clause) = signals[node].1 {
                if let Some(&dep_idx) = name_to_idx.get(clause.metric_ref.as_str()) {
                    if visited[dep_idx] == 1 {
                        // Cycle found: trace back from node to dep_idx.
                        let mut cycle = vec![signals[dep_idx].0.clone()];
                        let mut cur = node;
                        while cur != dep_idx {
                            cycle.push(signals[cur].0.clone());
                            cur = parent[cur];
                            if cur == usize::MAX {
                                break;
                            }
                        }
                        cycle.push(signals[dep_idx].0.clone());
                        cycle.reverse();
                        return cycle.join(" -> ");
                    }
                    if visited[dep_idx] == 0 {
                        parent[dep_idx] = node;
                        stack.push((dep_idx, false));
                    }
                }
            }
        }
    }

    "unknown cycle".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // AfterClause parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_less_than() {
        let clause = parse_after_clause("interface_oper_state < 1").expect("should parse");
        assert_eq!(clause.metric_ref, "interface_oper_state");
        assert_eq!(clause.operator, Operator::LessThan);
        assert!((clause.threshold - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_greater_than() {
        let clause = parse_after_clause("backup_link_utilization > 70").expect("should parse");
        assert_eq!(clause.metric_ref, "backup_link_utilization");
        assert_eq!(clause.operator, Operator::GreaterThan);
        assert!((clause.threshold - 70.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_with_extra_whitespace() {
        let clause =
            parse_after_clause("  metric_name   >   42.5  ").expect("should parse with spaces");
        assert_eq!(clause.metric_ref, "metric_name");
        assert_eq!(clause.operator, Operator::GreaterThan);
        assert!((clause.threshold - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_negative_threshold() {
        let clause = parse_after_clause("temp < -10").expect("should parse negative");
        assert_eq!(clause.metric_ref, "temp");
        assert_eq!(clause.operator, Operator::LessThan);
        assert!((clause.threshold - (-10.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_missing_operator() {
        let err = parse_after_clause("metric_name 70").expect_err("should fail");
        assert!(err.contains("no operator"), "got: {err}");
    }

    #[test]
    fn parse_missing_metric() {
        let err = parse_after_clause("< 70").expect_err("should fail");
        assert!(err.contains("no metric name"), "got: {err}");
    }

    #[test]
    fn parse_missing_threshold() {
        let err = parse_after_clause("metric >").expect_err("should fail");
        assert!(err.contains("no threshold"), "got: {err}");
    }

    #[test]
    fn parse_invalid_threshold() {
        let err = parse_after_clause("metric > abc").expect_err("should fail");
        assert!(err.contains("invalid threshold"), "got: {err}");
    }

    #[test]
    fn parse_multiple_operators() {
        let err = parse_after_clause("a < b > c").expect_err("should fail");
        assert!(err.contains("multiple operators"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Offset resolution
    // -----------------------------------------------------------------------

    fn make_params(behavior: &str, kvs: &[(&str, serde_yaml_ng::Value)]) -> SignalParams {
        let mut params = HashMap::new();
        for (k, v) in kvs {
            params.insert(k.to_string(), v.clone());
        }
        SignalParams {
            behavior: behavior.to_string(),
            params,
        }
    }

    fn sv(s: &str) -> serde_yaml_ng::Value {
        serde_yaml_ng::Value::String(s.to_string())
    }

    fn nv(n: f64) -> serde_yaml_ng::Value {
        serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(n))
    }

    #[test]
    fn no_after_clauses_all_zero() {
        let signals = vec![
            ("metric_a".to_string(), None, make_params("flap", &[])),
            ("metric_b".to_string(), None, make_params("saturation", &[])),
        ];

        let offsets = resolve_offsets(&signals).expect("should succeed");
        assert!((offsets["metric_a"]).abs() < f64::EPSILON);
        assert!((offsets["metric_b"]).abs() < f64::EPSILON);
    }

    #[test]
    fn simple_dependency_chain() {
        // A (flap, up_duration=60s) -> B depends on "A < 1" -> offset = 60s
        let signals = vec![
            (
                "interface_oper_state".to_string(),
                None,
                make_params("flap", &[("up_duration", sv("60s"))]),
            ),
            (
                "backup_link_utilization".to_string(),
                Some(AfterClause {
                    metric_ref: "interface_oper_state".to_string(),
                    operator: Operator::LessThan,
                    threshold: 1.0,
                }),
                make_params(
                    "saturation",
                    &[
                        ("baseline", nv(20.0)),
                        ("ceiling", nv(85.0)),
                        ("time_to_saturate", sv("2m")),
                    ],
                ),
            ),
        ];

        let offsets = resolve_offsets(&signals).expect("should succeed");
        assert!((offsets["interface_oper_state"]).abs() < f64::EPSILON);
        assert!((offsets["backup_link_utilization"] - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn transitive_dependency() {
        // A (flap, up_duration=60s) at t=0
        // B depends on "A < 1" -> offset = 60s
        // B (saturation, baseline=20, ceiling=85, period=120s)
        // C depends on "B > 70" -> offset = 60 + (70-20)/(85-20)*120 = 60 + 92.307...
        let signals = vec![
            (
                "interface_oper_state".to_string(),
                None,
                make_params("flap", &[("up_duration", sv("60s"))]),
            ),
            (
                "backup_link_utilization".to_string(),
                Some(AfterClause {
                    metric_ref: "interface_oper_state".to_string(),
                    operator: Operator::LessThan,
                    threshold: 1.0,
                }),
                make_params(
                    "saturation",
                    &[
                        ("baseline", nv(20.0)),
                        ("ceiling", nv(85.0)),
                        ("time_to_saturate", sv("120s")),
                    ],
                ),
            ),
            (
                "latency_ms".to_string(),
                Some(AfterClause {
                    metric_ref: "backup_link_utilization".to_string(),
                    operator: Operator::GreaterThan,
                    threshold: 70.0,
                }),
                make_params(
                    "degradation",
                    &[
                        ("baseline", nv(5.0)),
                        ("ceiling", nv(150.0)),
                        ("time_to_degrade", sv("3m")),
                    ],
                ),
            ),
        ];

        let offsets = resolve_offsets(&signals).expect("should succeed");
        let expected_b = 60.0;
        let expected_c = 60.0 + (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert!((offsets["interface_oper_state"]).abs() < f64::EPSILON);
        assert!(
            (offsets["backup_link_utilization"] - expected_b).abs() < f64::EPSILON,
            "B: got {}, expected {expected_b}",
            offsets["backup_link_utilization"]
        );
        assert!(
            (offsets["latency_ms"] - expected_c).abs() < 1e-9,
            "C: got {}, expected {expected_c}",
            offsets["latency_ms"]
        );
    }

    #[test]
    fn unknown_metric_reference() {
        let signals = vec![(
            "metric_a".to_string(),
            Some(AfterClause {
                metric_ref: "nonexistent".to_string(),
                operator: Operator::LessThan,
                threshold: 1.0,
            }),
            make_params("flap", &[]),
        )];

        let err = resolve_offsets(&signals).expect_err("should fail");
        assert!(
            err.contains("nonexistent") && err.contains("not defined"),
            "got: {err}"
        );
    }

    #[test]
    fn cycle_detection() {
        let signals = vec![
            (
                "a".to_string(),
                Some(AfterClause {
                    metric_ref: "b".to_string(),
                    operator: Operator::LessThan,
                    threshold: 1.0,
                }),
                make_params("flap", &[]),
            ),
            (
                "b".to_string(),
                Some(AfterClause {
                    metric_ref: "a".to_string(),
                    operator: Operator::LessThan,
                    threshold: 1.0,
                }),
                make_params("flap", &[]),
            ),
        ];

        let err = resolve_offsets(&signals).expect_err("should fail");
        assert!(err.contains("circular dependency"), "got: {err}");
    }

    #[test]
    fn steady_behavior_rejected() {
        let signals = vec![
            (
                "baseline_metric".to_string(),
                None,
                make_params("steady", &[]),
            ),
            (
                "dependent".to_string(),
                Some(AfterClause {
                    metric_ref: "baseline_metric".to_string(),
                    operator: Operator::GreaterThan,
                    threshold: 50.0,
                }),
                make_params("saturation", &[]),
            ),
        ];

        let err = resolve_offsets(&signals).expect_err("should fail");
        assert!(err.contains("steady"), "got: {err}");
    }

    #[test]
    fn out_of_range_threshold() {
        // saturation: baseline=20, ceiling=85. Threshold 150 is out of range.
        let signals = vec![
            (
                "util".to_string(),
                None,
                make_params(
                    "saturation",
                    &[("baseline", nv(20.0)), ("ceiling", nv(85.0))],
                ),
            ),
            (
                "dependent".to_string(),
                Some(AfterClause {
                    metric_ref: "util".to_string(),
                    operator: Operator::GreaterThan,
                    threshold: 150.0,
                }),
                make_params("flap", &[]),
            ),
        ];

        let err = resolve_offsets(&signals).expect_err("should fail");
        assert!(
            err.contains("150") && (err.contains("ceiling") || err.contains("exceeds")),
            "got: {err}"
        );
    }
}
