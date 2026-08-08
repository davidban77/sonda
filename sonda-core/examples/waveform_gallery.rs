//! Generates the SVG waveform gallery embedded in the documentation.
//!
//! Each figure is produced by the real config path — YAML is parsed into a
//! [`ScenarioConfig`], operational aliases are desugared exactly as `sonda run`
//! would, and the resulting generator is sampled tick by tick — so the shapes
//! on the site can never drift from what the code emits.
//!
//! Run from the workspace root (or via `task site:waveforms`):
//!
//! ```bash
//! cargo run -p sonda-core --example waveform_gallery
//! ```
//!
//! Output: `docs/site/docs/build/img/generators/<type>.svg`, committed to git.

use sonda_core::config::aliases::desugar_scenario_config;
use sonda_core::config::ScenarioConfig;
use sonda_core::generator::{create_generator, JitterWrapper, ValueGenerator};
use std::fmt::Write as _;
use std::path::PathBuf;

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 140.0;
const PAD_X: f64 = 10.0;
const PAD_Y: f64 = 14.0;
/// Coral trace — legible on both the light and dark doc themes.
const STROKE: &str = "#f97316";
const GRID: &str = "#94a3b8";

/// One gallery figure: output slug, sample count, whether to draw the trace
/// as discrete steps (tick-quantized signals) or a continuous line, and the
/// scenario YAML fed through the real config path.
struct Figure {
    slug: &'static str,
    samples: u64,
    stepped: bool,
    yaml: &'static str,
}

const FIGURES: &[Figure] = &[
    // -- Core metric generators ------------------------------------------
    Figure {
        slug: "constant",
        samples: 120,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: constant\n  value: 50.0\n",
    },
    Figure {
        slug: "sine",
        samples: 180,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: sine\n  amplitude: 40.0\n  offset: 50.0\n  period_secs: 15\n",
    },
    Figure {
        slug: "sawtooth",
        samples: 180,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: sawtooth\n  min: 10.0\n  max: 90.0\n  period_secs: 15\n",
    },
    Figure {
        slug: "uniform",
        samples: 120,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: uniform\n  min: 20.0\n  max: 80.0\n  seed: 42\n",
    },
    Figure {
        slug: "sequence",
        samples: 64,
        stepped: true,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: sequence\n  values: [15.0, 15.0, 15.0, 15.0, 45.0, 45.0, 45.0, 45.0, 80.0, 80.0, 80.0, 80.0, 55.0, 55.0, 55.0, 55.0]\n",
    },
    Figure {
        slug: "step",
        samples: 130,
        stepped: true,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: step\n  start: 0.0\n  step_size: 2.0\n  max: 100.0\n",
    },
    Figure {
        slug: "spike",
        samples: 180,
        stepped: true,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: spike\n  baseline: 20.0\n  magnitude: 60.0\n  duration_secs: 2\n  interval_secs: 15\n",
    },
    // -- Operational aliases (desugared exactly as `sonda run` would) ----
    Figure {
        slug: "steady",
        samples: 240,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: steady\n  center: 50.0\n  amplitude: 10.0\n  period: 20s\n  noise: 2.0\n  noise_seed: 7\n",
    },
    Figure {
        slug: "flap",
        samples: 120,
        stepped: true,
        yaml: "name: g\nrate: 2\ngenerator:\n  type: flap\n  up_duration: 10s\n  down_duration: 5s\n",
    },
    Figure {
        slug: "saturation",
        samples: 240,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: saturation\n  baseline: 10.0\n  ceiling: 90.0\n  time_to_saturate: 20s\n",
    },
    Figure {
        slug: "leak",
        samples: 240,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: leak\n  baseline: 10.0\n  ceiling: 90.0\n  time_to_ceiling: 60s\n",
    },
    Figure {
        slug: "degradation",
        samples: 240,
        stepped: false,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: degradation\n  baseline: 20.0\n  ceiling: 80.0\n  time_to_degrade: 30s\n  noise: 3.0\n  noise_seed: 11\n",
    },
    Figure {
        slug: "spike_event",
        samples: 240,
        stepped: true,
        yaml: "name: g\nrate: 4\ngenerator:\n  type: spike_event\n  baseline: 10.0\n  spike_height: 70.0\n  spike_duration: 3s\n  spike_interval: 20s\n",
    },
];

fn main() {
    let out_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/site/docs/build/img/generators");
    std::fs::create_dir_all(&out_dir).expect("create output directory");

    for figure in FIGURES {
        let values = sample(figure);
        let svg = render_svg(&values, figure.stepped, figure.slug);
        let path = out_dir.join(format!("{}.svg", figure.slug));
        std::fs::write(&path, svg).expect("write SVG file");
        println!("wrote {}", path.display());
    }
}

/// Parse the figure's YAML, desugar aliases, and sample the generator —
/// the same path `sonda run` takes to turn config into values.
fn sample(figure: &Figure) -> Vec<f64> {
    let mut config: ScenarioConfig =
        serde_yaml_ng::from_str(figure.yaml).expect("gallery YAML must parse");
    desugar_scenario_config(&mut config).expect("gallery YAML must desugar");

    let generator = create_generator(&config.generator, config.base.rate)
        .expect("gallery config must be valid");
    let generator: Box<dyn ValueGenerator> = match config.base.jitter {
        Some(jitter) if jitter > 0.0 => Box::new(JitterWrapper::new(
            generator,
            jitter,
            config.base.jitter_seed.unwrap_or(0),
        )),
        _ => generator,
    };

    (0..figure.samples)
        .map(|tick| generator.value(tick))
        .collect()
}

/// Render sampled values as a small self-contained SVG chart.
///
/// Transparent background with a mid-grey dashed grid and a coral trace, so
/// the figure reads on both the light and dark documentation themes.
fn render_svg(values: &[f64], stepped: bool, slug: &str) -> String {
    let (min, max) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    // Flat signals (constant) still get a visible band to sit in.
    let span = if (max - min).abs() < 1e-9 {
        1.0
    } else {
        max - min
    };
    let (min, span) = (min - span * 0.08, span * 1.16);

    let x = |i: usize| PAD_X + (i as f64 / (values.len() - 1) as f64) * (WIDTH - 2.0 * PAD_X);
    let y = |v: f64| HEIGHT - PAD_Y - ((v - min) / span) * (HEIGHT - 2.0 * PAD_Y);

    let mut path = String::new();
    for (i, &v) in values.iter().enumerate() {
        if i == 0 {
            let _ = write!(path, "M{:.1} {:.1}", x(i), y(v));
        } else if stepped {
            // Horizontal-then-vertical moves: tick-quantized signals render
            // as crisp plateaus instead of misleading diagonals.
            let _ = write!(path, " H{:.1} V{:.1}", x(i), y(v));
        } else {
            let _ = write!(path, " L{:.1} {:.1}", x(i), y(v));
        }
    }

    let grid_rows: String = (1..4)
        .map(|row| {
            let gy = PAD_Y + (row as f64 / 4.0) * (HEIGHT - 2.0 * PAD_Y);
            format!(
                "<line x1=\"{PAD_X}\" y1=\"{gy:.1}\" x2=\"{:.1}\" y2=\"{gy:.1}\" \
                 stroke=\"{GRID}\" stroke-opacity=\"0.3\" stroke-width=\"1\" \
                 stroke-dasharray=\"2 5\"/>",
                WIDTH - PAD_X
            )
        })
        .collect();

    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {HEIGHT}\" \
         role=\"img\" aria-label=\"Waveform produced by the {slug} generator\">\
         {grid_rows}\
         <path d=\"{path}\" fill=\"none\" stroke=\"{STROKE}\" stroke-width=\"2.2\" \
         stroke-linejoin=\"round\" stroke-linecap=\"round\"/>\
         </svg>\n"
    )
}
