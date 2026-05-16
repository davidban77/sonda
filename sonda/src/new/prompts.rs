//! Minimal interactive prompts for `sonda new`.

use anyhow::Result;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Select};

#[derive(Debug, Clone, Copy)]
pub enum SignalKind {
    Metrics,
    Logs,
    Histogram,
    Summary,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::Metrics => "metrics",
            SignalKind::Logs => "logs",
            SignalKind::Histogram => "histogram",
            SignalKind::Summary => "summary",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SinkKind {
    Stdout,
    File { path: String },
    HttpPush { endpoint: String },
}

pub struct Answers {
    pub id: String,
    pub signal: SignalKind,
    pub generator_type: String,
    pub rate: f64,
    pub duration: String,
    pub sink: SinkKind,
}

pub fn collect_answers() -> Result<Answers> {
    let theme = ColorfulTheme::default();

    let signal_choices = ["metrics", "logs", "histogram", "summary"];
    let signal_idx = Select::with_theme(&theme)
        .with_prompt("Signal type")
        .items(signal_choices)
        .default(0)
        .interact()?;
    let signal = match signal_idx {
        0 => SignalKind::Metrics,
        1 => SignalKind::Logs,
        2 => SignalKind::Histogram,
        _ => SignalKind::Summary,
    };

    let id: String = Input::with_theme(&theme)
        .with_prompt("Scenario id")
        .default("example".to_string())
        .interact_text()?;

    let generator_type = match signal {
        SignalKind::Metrics => {
            let gen_choices = ["constant", "sine", "sawtooth", "uniform", "steady"];
            let idx = Select::with_theme(&theme)
                .with_prompt("Generator")
                .items(gen_choices)
                .default(0)
                .interact()?;
            gen_choices[idx].to_string()
        }
        _ => String::new(),
    };

    let rate_str: String = Input::with_theme(&theme)
        .with_prompt("Events per second")
        .default("1".to_string())
        .validate_with(|input: &String| -> Result<(), String> {
            input
                .parse::<f64>()
                .map_err(|_| "must be a number".to_string())
                .and_then(|v| {
                    if v > 0.0 {
                        Ok(())
                    } else {
                        Err("must be positive".to_string())
                    }
                })
        })
        .interact_text()?;
    let rate: f64 = rate_str.parse().expect("validated above");

    let duration: String = Input::with_theme(&theme)
        .with_prompt("Duration (e.g. 60s, 5m)")
        .default("60s".to_string())
        .interact_text()?;

    let sink_choices = ["stdout", "file", "http_push"];
    let sink_idx = Select::with_theme(&theme)
        .with_prompt("Sink")
        .items(sink_choices)
        .default(0)
        .interact()?;
    let sink = match sink_idx {
        0 => SinkKind::Stdout,
        1 => {
            let path: String = Input::with_theme(&theme)
                .with_prompt("File path")
                .default("./out.txt".to_string())
                .interact_text()?;
            SinkKind::File { path }
        }
        _ => {
            let endpoint: String = Input::with_theme(&theme)
                .with_prompt("HTTP endpoint URL")
                .interact_text()?;
            SinkKind::HttpPush { endpoint }
        }
    };

    Ok(Answers {
        id,
        signal,
        generator_type,
        rate,
        duration,
        sink,
    })
}
