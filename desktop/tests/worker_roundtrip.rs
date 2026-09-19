//! End-to-end round-trip against the real Python worker.
//!
//! Uses the repo `.venv` interpreter when present so `cargo test` exercises
//! the actual protocol, not a mock. Skips gracefully when the interpreter is
//! missing (e.g. a Rust-only checkout); the unit tests in `protocol` still run.

use std::path::PathBuf;
use teamup_shift_sync_gui::worker::{AppFiles, WorkerHandle};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn dev_files() -> Option<AppFiles> {
    let root = repo_root();
    let python = root.join(".venv/bin/python");
    if !python.is_file() {
        return None;
    }
    // Point the supervisor at the dev interpreter explicitly.
    std::env::set_var(
        "TEAMUP_WORKER_CMD",
        format!("{} -m teamup_shift_sync.worker", python.display()),
    );
    std::env::set_var(
        "TEAMUP_SHIFT_SYNC_CONFIG",
        root.join("fixtures/offline-config.toml"),
    );
    std::env::set_var(
        "TEAMUP_FIXTURE",
        root.join("fixtures/representative-week.json"),
    );
    let data_dir = root.join(".local/gui-roundtrip");
    std::env::set_var("TEAMUP_SHIFT_SYNC_DATA_DIR", &data_dir);
    Some(AppFiles::resolve())
}

#[test]
fn worker_roundtrip_home_and_fixture_preview() {
    let Some(files) = dev_files() else {
        eprintln!("SKIP: repo .venv interpreter not found");
        return;
    };
    assert!(files.is_fixture, "dev fixture must exist");

    let worker = WorkerHandle::spawn(&files).expect("worker must spawn and answer ping");

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
