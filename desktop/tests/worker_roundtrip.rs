//! End-to-end round-trip against the real Python worker.
//!
//! Tries every plausible interpreter in order — an explicit
//! `TEAMUP_TEST_PYTHON` override, the repo `.venv` (POSIX and Windows
//! layouts), then `python3`/`python` on `PATH` — and runs the assertions
//! against the first one that answers the protocol handshake. Skips
//! gracefully only when no interpreter can serve the worker (e.g. a
//! Rust-only checkout); the unit tests in `protocol` still run.

use std::path::{Path, PathBuf};
use teamup_shift_sync_gui::worker::{AppFiles, WorkerHandle};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn worker_commands(root: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if let Ok(python) = std::env::var("TEAMUP_TEST_PYTHON") {
        commands.push(format!("{python} -m teamup_shift_sync.worker"));
    }
    for candidate in [
        root.join(".venv/bin/python"),
        root.join(".venv/Scripts/python.exe"),
    ] {
        if candidate.is_file() {
            commands.push(format!(
                "{} -m teamup_shift_sync.worker",
                candidate.display()
            ));
        }
    }
    // CI installs the package into its own interpreter, so a bare
    // `python3`/`python` on PATH can serve the worker there.
    commands.push("python3 -m teamup_shift_sync.worker".to_string());
    commands.push("python -m teamup_shift_sync.worker".to_string());
    commands
}

fn dev_files() -> AppFiles {
    let root = repo_root();
    std::env::set_var(
        "TEAMUP_SHIFT_SYNC_CONFIG",
        root.join("fixtures/offline-config.toml"),
    );
    std::env::set_var(
        "TEAMUP_FIXTURE",
        root.join("fixtures/representative-week.json"),
    );
    std::env::set_var(
        "TEAMUP_SHIFT_SYNC_DATA_DIR",
        root.join(".local/gui-roundtrip"),
    );
    AppFiles::resolve()
}

#[test]
fn worker_roundtrip_home_and_fixture_preview() {
    let root = repo_root();
    let files = dev_files();
    assert!(files.is_fixture, "dev fixture must exist");

    let mut worker = None;
    for command in worker_commands(&root) {
        std::env::set_var("TEAMUP_WORKER_CMD", &command);
        match WorkerHandle::spawn(&files) {
            Ok(handle) => {
                worker = Some(handle);
                break;
            }
            Err(error) => {
                eprintln!("worker candidate {command:?} unavailable: {error}");
            }
        }
    }
    let Some(worker) = worker else {
        eprintln!("SKIP: no Python interpreter could serve the worker");
        return;
    };

    let status = worker
        .home_status(Some("2026-09-19T12:00:00+02:00"))
        .expect("home_status must succeed");
    assert!(status.config_issues.is_empty());
    assert_eq!(status.week_start, "2026-09-14");
    assert_eq!(status.login_state, "unknown");
    assert_eq!(status.last_verified_at, None);

    let preview = worker
        .preview_fixture("2026-09-14", "2026-09-20")
        .expect("fixture preview must succeed");
    assert!(!preview.items.is_empty());
    assert!(!preview.digest.is_empty());
    assert_eq!(
        preview.counts.values().sum::<u64>(),
        preview.items.len() as u64
    );
}
