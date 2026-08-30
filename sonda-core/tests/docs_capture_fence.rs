//! The documented capture on `import/from-prometheus.md` must be what the tool
//! writes.
//!
//! That page shipped on one claim: its fences are generated, not written. The
//! claim was true and verified by hand once, and then decayed — a header added
//! to `to_yaml` put six lines into every real capture and left the page six
//! lines short, with nothing to notice. Verified-once is not a property; this
//! is.
//!
//! Drives the real emitter over the page's own stated fixture and compares
//! against the fence, so the two cannot drift apart again.

use sonda_core::acquire::normalize::{Grid, NormalizedSeries};
use sonda_core::acquire::yaml_out;
use std::collections::BTreeMap;

const PAGE: &str = "../docs/site/docs/import/from-prometheus.md";
const FENCE_TITLE: &str = "```yaml title=\"incident.yaml\"";

/// The fixture the page states in prose: a 15s step over 3615s, one `up`
/// series for `job="api"`, with the third sample absent.
///
/// Row 2 is the blank the page's `incident.csv` fence shows, and the one its
/// `gap_windows: [{at: 22.5s, for: 15s}]` covers.
const STEP_SECS: f64 = 15.0;
const ROWS: usize = 241;
const BLANK_ROW: usize = 2;

fn documented_capture() -> String {
    let mut labels = BTreeMap::new();
    labels.insert("__name__".to_string(), "up".to_string());
    labels.insert("job".to_string(), "api".to_string());

    let values: Vec<Option<f64>> = (0..ROWS)
        .map(|i| if i == BLANK_ROW { None } else { Some(1.0) })
        .collect();

    // `Grid::new` takes an inclusive range and counts the start point, so the
    // end is the LAST point rather than one step past it.
    let grid = Grid::new(0.0, (ROWS - 1) as f64 * STEP_SECS, STEP_SECS).expect("grid");
    assert_eq!(
        grid.len, ROWS,
        "the fixture must be the {ROWS} rows the page states"
    );
    let series = [NormalizedSeries { labels, values }];
    let file = yaml_out::scenario_for("incident.csv", grid, &series, 1.0).expect("scenario");
    yaml_out::to_yaml(&file).expect("yaml")
}

/// Pull the body of the ```` ```yaml title="incident.yaml" ```` fence.
fn documented_fence() -> String {
    let page = std::fs::read_to_string(PAGE)
        .unwrap_or_else(|e| panic!("cannot read {PAGE}: {e} — has the page moved?"));

    let start = page
        .find(FENCE_TITLE)
        .unwrap_or_else(|| panic!("{PAGE} has no {FENCE_TITLE} fence"));
    let after = &page[start + FENCE_TITLE.len()..];
    let end = after
        .find("\n```")
        .unwrap_or_else(|| panic!("{PAGE}: the incident.yaml fence is unterminated"));

    after[..end].trim_start_matches('\n').to_string()
}

#[test]
fn the_documented_capture_is_what_the_tool_writes() {
    let fence = documented_fence();
    assert!(
        !fence.trim().is_empty(),
        "the incident.yaml fence is empty — this check would pass on nothing"
    );

    let real = documented_capture();
    assert!(
        real.contains("version: 2"),
        "the emitter produced nothing recognisable; the fixture is wrong, not the page"
    );

    if fence.trim_end() != real.trim_end() && std::env::var("SONDA_UPDATE_DOCS").is_ok() {
        rewrite_fence(&real);
        panic!("{PAGE} rewritten from the emitter — re-run without SONDA_UPDATE_DOCS");
    }

    assert_eq!(
        fence.trim_end(),
        real.trim_end(),
        "\n{PAGE}'s incident.yaml fence is not what `to_yaml` writes.\n\
         Regenerate with: SONDA_UPDATE_DOCS=1 cargo test -p sonda-core --test docs_capture_fence\n\
         Do not edit the fence by hand — that is how it drifted before.\n"
    );
}

/// Replace the fence body with `real`, so the page is machine-written rather
/// than transcribed. Mirrors `INSTA_UPDATE` for the same reason: a fence a
/// human retypes is a fence that drifts.
fn rewrite_fence(real: &str) {
    let page = std::fs::read_to_string(PAGE).expect("page");
    let start = page.find(FENCE_TITLE).expect("fence");
    let body_at = start + FENCE_TITLE.len() + 1;
    let end = body_at + page[body_at..].find("\n```").expect("fence end");
    let updated = format!("{}{}{}", &page[..body_at], real.trim_end(), &page[end..]);
    std::fs::write(PAGE, updated).expect("write page");
}

/// The page states the fixture in prose; if that prose changes, the constants
/// above stop describing it and the comparison silently tests the wrong file.
#[test]
fn the_page_still_states_the_fixture_this_test_builds() {
    let page = std::fs::read_to_string(PAGE).expect("page");

    for needle in ["--step 15s", "incident.csv", "job=\"\"api\"\""] {
        assert!(
            page.contains(needle),
            "{PAGE} no longer contains {needle:?} — the fixture in this test may no longer \
             match the one the page documents"
        );
    }
}
