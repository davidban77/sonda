//! Parse a Prometheus `query_range` matrix response into [`FetchedSeries`].
//!
//! Pure and feature-free, so the whole response surface — every malformed
//! shape a server can return — is unit-testable without a network or the
//! `http` feature. [`super::tsdb`] does nothing but fetch the bytes and hand
//! them here.
//!
//! The shape is the Prometheus HTTP API v1 `matrix` result, shared verbatim by
//! VictoriaMetrics, Mimir, Thanos and Cortex:
//!
//! ```text
//! {"status":"success","data":{"resultType":"matrix","result":[
//!    {"metric":{"__name__":"up","job":"api"},
//!     "values":[[1700000000,"1"],[1700000030,"0"]]}]}}
//! ```
//!
//! Sample values arrive as *strings* — that is the API, not a quirk — and
//! carry `NaN`, `+Inf` and `-Inf` for the non-finite cases. They are parsed
//! with `f64::from_str` and kept verbatim; nothing is clamped or dropped for
//! being unusual, because the point of this path is to replay what was there.

use super::FetchedSeries;
use std::collections::BTreeMap;

/// Parse a matrix response body.
///
/// Returns the series in the order the server listed them, each with samples
/// in ascending time order.
///
/// # Errors
///
/// Returns a human-readable reason. The caller wraps it with the URL — this
/// function deliberately knows nothing about where the bytes came from.
pub fn parse_matrix_response(body: &str) -> Result<Vec<FetchedSeries>, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;

    if parsed["status"] != "success" {
        // Prometheus puts a usable message in `error` on failures — a PromQL
        // syntax error should reach the user as itself, not as "bad response".
        let detail = parsed["error"]
            .as_str()
            .map(|e| format!(": {e}"))
            .unwrap_or_default();
        let kind = parsed["errorType"].as_str().unwrap_or("unknown");
        return Err(format!(
            "query failed (status {:?}, errorType {kind}){detail}",
            parsed["status"].as_str().unwrap_or("missing")
        ));
    }

    let result_type = parsed["data"]["resultType"].as_str().unwrap_or("missing");
    if result_type != "matrix" {
        return Err(format!(
            "expected resultType \"matrix\", got {result_type:?} — \
             a range query returns a matrix; an instant query returns a vector"
        ));
    }

    let result = parsed["data"]["result"]
        .as_array()
        .ok_or_else(|| "data.result is not an array".to_string())?;

    let mut out = Vec::with_capacity(result.len());
    for (i, entry) in result.iter().enumerate() {
        let mut labels = BTreeMap::new();
        if let Some(metric) = entry["metric"].as_object() {
            for (k, v) in metric {
                let v = v
                    .as_str()
                    .ok_or_else(|| format!("series {i}: label {k:?} is not a string"))?;
                labels.insert(k.clone(), v.to_string());
            }
        }

        let values = entry["values"]
            .as_array()
            .ok_or_else(|| format!("series {i}: \"values\" is missing or not an array"))?;

        let mut samples = Vec::with_capacity(values.len());
        for (j, pair) in values.iter().enumerate() {
            let pair = pair
                .as_array()
                .ok_or_else(|| format!("series {i} sample {j}: not a [timestamp, value] pair"))?;
            if pair.len() != 2 {
                return Err(format!(
                    "series {i} sample {j}: expected 2 elements, got {}",
                    pair.len()
                ));
            }
            let ts = pair[0]
                .as_f64()
                .ok_or_else(|| format!("series {i} sample {j}: timestamp is not a number"))?;
            let raw = pair[1]
                .as_str()
                .ok_or_else(|| format!("series {i} sample {j}: value is not a string"))?;
            let value: f64 = raw.parse().map_err(|_| {
                format!("series {i} sample {j}: value {raw:?} does not parse as a number")
            })?;
            samples.push((ts, value));
        }

        out.push(FetchedSeries { labels, samples });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{"status":"success","data":{"resultType":"matrix","result":[
        {"metric":{"__name__":"up","job":"api"},
         "values":[[1700000000,"1"],[1700000030,"0.5"]]}]}}"#;

    #[test]
    fn parses_labels_and_samples() {
        let s = parse_matrix_response(OK).expect("valid body");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].metric_name(), Some("up"));
        assert_eq!(s[0].samples, vec![(1700000000.0, 1.0), (1700000030.0, 0.5)]);
        let rest: Vec<(&str, &str)> = s[0].labels_without_name().collect();
        assert_eq!(rest, vec![("job", "api")]);
    }

    #[test]
    fn an_empty_result_is_success_with_no_series() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        assert_eq!(parse_matrix_response(body).expect("valid").len(), 0);
    }

    #[test]
    fn non_finite_values_survive_verbatim() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{},"values":[[1,"NaN"],[2,"+Inf"],[3,"-Inf"]]}]}}"#;
        let s = parse_matrix_response(body).expect("valid body");
        assert!(s[0].samples[0].1.is_nan());
        assert_eq!(s[0].samples[1].1, f64::INFINITY);
        assert_eq!(s[0].samples[2].1, f64::NEG_INFINITY);
    }

    #[test]
    fn a_query_error_surfaces_the_servers_own_message() {
        let body =
            r#"{"status":"error","errorType":"bad_data","error":"parse error: unexpected \")\""}"#;
        let e = parse_matrix_response(body).expect_err("must fail");
        assert!(e.contains("bad_data"), "keeps errorType: {e}");
        assert!(e.contains("parse error"), "keeps the message: {e}");
    }

    #[test]
    fn an_instant_query_result_is_rejected_with_a_useful_reason() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let e = parse_matrix_response(body).expect_err("must fail");
        assert!(e.contains("matrix"), "{e}");
        assert!(e.contains("vector"), "{e}");
    }

    #[test]
    fn malformed_bodies_produce_clean_errors_not_panics() {
        let cases: &[(&str, &str)] = &[
            ("not json", "{"),
            ("html error page", "<html>502 Bad Gateway</html>"),
            ("empty", ""),
            (
                "result not an array",
                r#"{"status":"success","data":{"resultType":"matrix","result":{}}}"#,
            ),
            (
                "values missing",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{}}]}}"#,
            ),
            (
                "sample not a pair",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1]]}]}}"#,
            ),
            (
                "timestamp not numeric",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[["x","1"]]}]}}"#,
            ),
            (
                "value not a string",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1,1]]}]}}"#,
            ),
            (
                "value unparseable",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1,"abc"]]}]}}"#,
            ),
            (
                "label not a string",
                r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"job":3},"values":[]}]}}"#,
            ),
        ];
        for (case, body) in cases {
            let e = parse_matrix_response(body);
            assert!(e.is_err(), "case {case}: must be an error");
            let msg = e.unwrap_err();
            assert!(!msg.is_empty(), "case {case}: error must say something");
        }
    }
}
