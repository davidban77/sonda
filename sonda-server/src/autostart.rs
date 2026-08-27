//! Startup sweep that admits every `kind: runnable` catalog entry.

use std::future::Future;
use std::path::Path;

use sonda_core::catalog::{self, CatalogEntry, CatalogError, EntryKind};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::routes::scenarios::{admit_compiled, compile_v2_text};
use crate::state::AppState;

/// Enumerate the catalog's runnable entries in the order the sweep will start them.
/// A catalog the operator must fix (duplicate or underivable entry name) is an error;
/// a directory that cannot be scanned warns and yields no entries, leaving the server
/// free to come up.
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
/// skipping any entry that fails so one bad file cannot stop the server. An entry
/// whose admission panics is skipped the same way: it runs in its own task, so the
/// panic ends that entry and not the sweep.
/// Runs alongside the HTTP server; `cancel` stops it at the next entry boundary
/// so shutdown never races an admission it would not see.
pub async fn start_entries(state: &AppState, entries: &[CatalogEntry], cancel: &CancellationToken) {
    start_entries_with(state, entries, cancel, admit_entry).await
}

async fn start_entries_with<F, Fut>(
    state: &AppState,
    entries: &[CatalogEntry],
    cancel: &CancellationToken,
    admit: F,
) where
    F: Fn(AppState, CatalogEntry) -> Fut,
    Fut: Future<Output = bool> + Send + 'static,
{
    for (index, entry) in entries.iter().enumerate() {
        if cancel.is_cancelled() {
            let started = state.sweep_status.snapshot().started;
            warn!(
                started,
                total = entries.len(),
                remaining = entries.len() - index,
                "autostart: shutdown signalled, stopped after starting {started} of {} runnable catalog entries",
                entries.len()
            );
            return;
        }

        match tokio::spawn(admit(state.clone(), entry.clone())).await {
            Ok(true) => state.sweep_status.record_started(),
            Ok(false) => {}
            Err(e) => {
                let path = entry.source_path.display().to_string();
                warn!(origin = %path, reason = %e, "{path}: panicked while starting, skipping catalog entry");
            }
        }
    }

    // Finish before the summary line, so anything that waits on the log sees a ready /ready.
    state.sweep_status.finish();
    let started = state.sweep_status.snapshot().started;
    info!(
        started,
        total = entries.len(),
        "autostart: started {started} of {} runnable catalog entries",
        entries.len()
    );
}

async fn admit_entry(state: AppState, entry: CatalogEntry) -> bool {
    let path = entry.source_path.display().to_string();

    let text = match std::fs::read_to_string(&entry.source_path) {
        Ok(text) => text,
        Err(e) => {
            warn!(origin = %path, reason = %e, "{path}: unreadable, skipping catalog entry");
            return false;
        }
    };

    let catalog_dir = state.catalog_dir.as_deref().map(|p| p.as_path());
    let compiled = match compile_v2_text(&text, catalog_dir) {
        Ok(compiled) => compiled,
        Err(fail) => {
            warn!(origin = %path, reason = %fail.message(), "{path}: does not compile, skipping catalog entry");
            return false;
        }
    };

    // admit_compiled logs the reason it rejected an entry, with the same origin.
    admit_compiled(&state, compiled, &path).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::state::{AppState, SweepPhase, SweepStatus};

    const RUNNABLE: &str = "\
version: 2
kind: runnable
scenario_name: alpha
defaults:
  rate: 1
  duration: 300s
  encoder:
    type: prometheus_text
  sink:
    type: memory
scenarios:
  - signal_type: metrics
    name: alpha_cpu
    generator:
      type: constant
      value: 1.0
";

    fn catalog_with_one_runnable() -> (TempDir, Vec<CatalogEntry>, AppState) {
        let dir = TempDir::new().expect("must create temp catalog dir");
        std::fs::write(dir.path().join("alpha.yaml"), RUNNABLE).expect("must write catalog file");
        let entries = runnable_entries(dir.path()).expect("must enumerate");
        assert_eq!(entries.len(), 1);

        let mut state = AppState::new();
        state.catalog_dir = Some(Arc::new(dir.path().to_path_buf()));
        state.sweep_status = Arc::new(SweepStatus::in_progress(entries.len()));
        (dir, entries, state)
    }

    fn runnable_named(name: &str) -> String {
        RUNNABLE
            .replace("scenario_name: alpha", &format!("scenario_name: {name}"))
            .replace("name: alpha_cpu", &format!("name: {name}_cpu"))
    }

    fn catalog_with_three_runnables() -> (TempDir, Vec<CatalogEntry>, AppState) {
        let dir = TempDir::new().expect("must create temp catalog dir");
        for name in ["alpha", "beta", "gamma"] {
            std::fs::write(
                dir.path().join(format!("{name}.yaml")),
                runnable_named(name),
            )
            .expect("must write catalog file");
        }
        let entries = runnable_entries(dir.path()).expect("must enumerate");
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"],
            "the sweep must reach beta with an entry still behind it"
        );

        let mut state = AppState::new();
        state.catalog_dir = Some(Arc::new(dir.path().to_path_buf()));
        state.sweep_status = Arc::new(SweepStatus::in_progress(entries.len()));
        (dir, entries, state)
    }

    async fn admit_but_panic_on_beta(state: AppState, entry: CatalogEntry) -> bool {
        if entry.name == "beta" {
            panic!("intentional admission panic");
        }
        admit_entry(state, entry).await
    }

    fn admitted(state: &AppState) -> usize {
        state.scenarios.read().len()
    }

    #[tokio::test]
    async fn sweep_admits_every_runnable_entry() {
        let (_dir, entries, state) = catalog_with_one_runnable();

        start_entries(&state, &entries, &CancellationToken::new()).await;

        assert_eq!(admitted(&state), 1);
    }

    #[tokio::test]
    async fn a_completed_sweep_reports_finished_with_its_counts() {
        let (_dir, entries, state) = catalog_with_one_runnable();

        start_entries(&state, &entries, &CancellationToken::new()).await;

        let snap = state.sweep_status.snapshot();
        assert_eq!(snap.phase, SweepPhase::Finished);
        assert_eq!((snap.started, snap.expected), (1, 1));
    }

    #[tokio::test]
    async fn entries_behind_a_panicking_admission_still_start() {
        let (_dir, entries, state) = catalog_with_three_runnables();

        start_entries_with(
            &state,
            &entries,
            &CancellationToken::new(),
            admit_but_panic_on_beta,
        )
        .await;

        let names: Vec<String> = state
            .scenarios
            .read()
            .values()
            .map(|h| h.name.clone())
            .collect();
        assert_eq!(names.len(), 2, "got {names:?}");
        assert!(names.iter().any(|n| n == "alpha_cpu"), "got {names:?}");
        assert!(names.iter().any(|n| n == "gamma_cpu"), "got {names:?}");
    }

    #[tokio::test]
    async fn a_sweep_with_a_panicking_entry_still_finishes_and_reports_ready() {
        let (_dir, entries, state) = catalog_with_three_runnables();

        start_entries_with(
            &state,
            &entries,
            &CancellationToken::new(),
            admit_but_panic_on_beta,
        )
        .await;

        let snap = state.sweep_status.snapshot();
        assert_eq!(snap.phase, SweepPhase::Finished);
        assert_eq!((snap.started, snap.expected), (2, 3));

        let ready = crate::routes::ready::ready(axum::extract::State(state.clone())).await;
        assert_eq!(ready.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn cancelled_sweep_admits_nothing() {
        let (_dir, entries, state) = catalog_with_one_runnable();
        let cancel = CancellationToken::new();
        cancel.cancel();

        start_entries(&state, &entries, &cancel).await;

        assert_eq!(admitted(&state), 0);
    }

    #[tokio::test]
    async fn a_cancelled_sweep_never_reports_itself_finished() {
        let (_dir, entries, state) = catalog_with_one_runnable();
        let cancel = CancellationToken::new();
        cancel.cancel();

        start_entries(&state, &entries, &cancel).await;

        assert_eq!(state.sweep_status.snapshot().phase, SweepPhase::InProgress);
    }
}
