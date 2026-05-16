//! `sonda new` — scaffold a v2 scenario YAML.

pub mod csv_reader;
pub mod prompts;
pub mod yaml_gen;

use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::NewArgs;

pub fn run(args: &NewArgs) -> Result<()> {
    let yaml = if args.template {
        yaml_gen::minimal_template()
    } else if let Some(ref csv_path) = args.from {
        scaffold_from_csv(csv_path)?
    } else {
        run_interactive()?
    };

    match args.output {
        Some(ref path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create parent dir {}", parent.display())
                    })?;
                }
            }
            fs::write(path, &yaml)
                .with_context(|| format!("failed to write {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => {
            print!("{yaml}");
        }
    }
    Ok(())
}

fn scaffold_from_csv(path: &Path) -> Result<String> {
    let data = csv_reader::read_csv(path, None)?;
    let mut specs = Vec::with_capacity(data.columns.len());
    for (col_idx, col) in data.columns.iter().enumerate() {
        let values = &data.values[col_idx];
        if values.is_empty() {
            continue;
        }
        let pattern = sonda_core::analysis::pattern::detect_pattern(values);
        specs.push(yaml_gen::spec_from_pattern(&pattern, col, 1.0, "60s"));
    }
    if specs.is_empty() {
        anyhow::bail!("no numeric data found in {}", path.display());
    }
    Ok(yaml_gen::render_v2(&specs, 1.0, "60s"))
}

fn run_interactive() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "sonda new requires an interactive terminal; pass --template or --from <csv> for non-interactive usage"
        );
    }
    let answers = prompts::collect_answers()?;
    Ok(yaml_gen::render_from_answers(&answers))
}
