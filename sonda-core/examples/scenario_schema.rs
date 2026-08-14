//! Emit the JSON Schema for the v2 scenario file format.
//!
//! ```console
//! $ cargo run -p sonda-core --all-features --example scenario_schema \
//!     -- docs/site/docs/schema/sonda-scenario.schema.json
//! ```
//!
//! With no argument it writes to stdout, which is what makes this usable as a
//! plain generator (`... --example scenario_schema > my.schema.json`) as well
//! as the thing `task schema:generate` calls.
//!
//! `--all-features` rather than `--features schema`: the delivery features add
//! config shape (a kafka sink's `tls:` and `sasl:` fields exist only with the
//! `kafka` feature), so a narrower build produces a schema that rejects
//! config the released binary accepts. See `sonda_core::schema` for the
//! measurement.
//!
//! The committed copy at `docs/site/docs/schema/sonda-scenario.schema.json` is a BUILD
//! OUTPUT. `task schema:check` regenerates it and fails on any difference, so
//! a change to the config types that is merged without rerunning this shows up
//! as a red gate rather than as a schema quietly describing the old format.

use std::io::Write;

fn main() -> std::io::Result<()> {
    let json = sonda_core::schema::scenario_file_schema_json();

    // `args().nth(1)`, not a flag parser: this is a one-argument generator and
    // sonda-core has no CLI dependency to spend on it.
    match std::env::args().nth(1) {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&path, json.as_bytes())?;
            eprintln!("wrote {path}");
        }
        None => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            out.write_all(json.as_bytes())?;
            out.flush()?;
        }
    }

    Ok(())
}
