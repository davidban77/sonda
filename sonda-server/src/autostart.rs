//! Startup sweep that admits every `kind: runnable` catalog entry.

use std::path::Path;

use sonda_core::catalog::{self, CatalogEntry, CatalogError, EntryKind};
use tracing::{info, warn};

use crate::routes::scenarios::{admit_compiled, compile_v2_text};
use crate::state::AppState;

/// Enumerate the catalog's runnable entries in the order the sweep will start them.
/// A catalog the operator must fix (duplicate or underivable entry name) is an error;
/// transient I/O warns and yields no entries, leaving the server free to come up.
pub fn runnable_entries(catalog_dir: &Path) -> anyhow::Result<Vec<CatalogEntry>> {
    let entries = match catalog::enumerate(catalog_dir) {
        Ok(entries) => entries,
        Err(e) if is_misconfiguration(&e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "--autostart: catalog {} is not startable",
                catalog_dir.display()
            )))
        }
        Err(e) => {
            let reason = format!("{:#}", anyhow::Error::new(e));
            warn!(catalog = %catalog_dir.display(), reason = %reason, "autostart: catalog could not be read, starting nothing");
            return Ok(Vec::new());
        }
    };

    Ok(entries
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::Runnable)
        .collect())
}

fn is_misconfiguration(error: &CatalogError) -> bool {
    match error {
        CatalogError::NotADirectory { .. }
        | CatalogError::InvalidName { .. }
        | CatalogError::DuplicateName { .. }
        | CatalogError::UnknownEntry { .. } => true,
        CatalogError::ReadDir { .. }
        | CatalogError::ReadEntry { .. }
        | CatalogError::ReadFile { .. } => false,
    }
}

/// Admit every entry through the same path `POST /scenarios` uses, logging and
/// skipping any entry that fails so one bad file cannot stop the server.
pub async fn start_entries(state: &AppState, entries: &[CatalogEntry]) {
    let catalog_dir = state.catalog_dir.as_deref().map(|p| p.as_path());
    let mut started = 0usize;

    for entry in entries {
        let path = entry.source_path.display().to_string();

        let text = match std::fs::read_to_string(&entry.source_path) {
            Ok(text) => text,
            Err(e) => {
                warn!(origin = %path, reason = %e, "{path}: unreadable, skipping catalog entry");
                continue;
            }
        };

        let compiled = match compile_v2_text(&text, catalog_dir) {
            Ok(compiled) => compiled,
            Err(fail) => {
                warn!(origin = %path, reason = %fail.message(), "{path}: does not compile, skipping catalog entry");
                continue;
            }
        };

        // admit_compiled logs the reason it rejected an entry, with the same origin.
        if admit_compiled(state, compiled, &path).await.is_ok() {
            started += 1;
        }
    }

    info!(
        started,
        total = entries.len(),
        "autostart: started {started} of {} runnable catalog entries",
        entries.len()
    );
}
