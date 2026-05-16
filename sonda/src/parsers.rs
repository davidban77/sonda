use anyhow::Context;

use crate::cli::{ParsersAction, ParsersArgs, RawlogArgs};

pub fn run(args: &ParsersArgs) -> anyhow::Result<()> {
    match &args.action {
        ParsersAction::Rawlog(rawlog_args) => run_rawlog(rawlog_args),
    }
}

fn run_rawlog(args: &RawlogArgs) -> anyhow::Result<()> {
    let parser_args = sonda_parsers::rawlog::RawlogArgs {
        input: args.file.clone(),
        format: args.format.clone(),
        output: args.output.clone(),
        delta_seconds: args.delta_seconds,
        scenario_name: args.scenario_name.clone(),
    };
    let output = sonda_parsers::rawlog::run(parser_args)
        .with_context(|| format!("rawlog parser failed for {:?}", args.file))?;

    let abs_yaml = output
        .yaml_path
        .canonicalize()
        .unwrap_or_else(|_| output.yaml_path.clone());
    let yaml_parent = abs_yaml
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    eprintln!(
        "parsed {} rows from {:?} (format: {})",
        output.row_count, args.file, output.format
    );
    eprintln!("wrote csv:  {}", output.csv_path.display());
    eprintln!("wrote yaml: {}", output.yaml_path.display());
    eprintln!(
        "validate:   sonda --dry-run run --scenario {}",
        abs_yaml.display()
    );
    eprintln!(
        "run:        (cd {yaml_parent} && sonda run --scenario {})",
        abs_yaml.display()
    );
    Ok(())
}
