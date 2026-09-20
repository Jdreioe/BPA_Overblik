//! End-to-end round-trip against the real Python worker.
//!
//! Tries every plausible interpreter in order — an explicit
//! `TEAMUP_TEST_PYTHON` override, the repo `.venv` (POSIX and Windows
//! layouts), then `python3`/`python` on `PATH` — and runs the assertions
//! against the first one that answers the protocol handshake. Skips
//! gracefully only when no interpreter can serve the worker (e.g. a
//! Rust-only checkout); the unit tests in `protocol` still run.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
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
    // `python3`/`python` on PATH can serve the worker there. On Windows
    // `python` goes first: `python3` may resolve to the Microsoft Store
    // stub, which waits on an unseen install prompt instead of answering
    // the handshake (a spawn timeout still bounds the damage, but skipping
    // the stub avoids a 60-second stall per run).
    if cfg!(windows) {
        commands.push("python -m teamup_shift_sync.worker".to_string());
        commands.push("python3 -m teamup_shift_sync.worker".to_string());
    } else {
        commands.push("python3 -m teamup_shift_sync.worker".to_string());
        commands.push("python -m teamup_shift_sync.worker".to_string());
    }
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

/// Run one worker stage with a bound, so a silent worker fails naming
/// the stuck stage instead of hanging the suite until the job times out.
fn guarded<T: Send + 'static>(
    stage: &'static str,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => panic!("worker_roundtrip stage '{stage}' failed: {error}"),
        Err(_) => panic!("worker_roundtrip stalled with no response in stage '{stage}'"),
    }
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

    let status = guarded("home_status", {
        let worker = worker.clone();
        move || {
            worker
                .home_status(Some("2026-09-19T12:00:00+02:00"))
                .map_err(|e| e.to_string())
        }
    });
    assert!(status.config_issues.is_empty());
    assert_eq!(status.week_start, "2026-09-14");
    assert_eq!(status.login_state, "unknown");
    assert_eq!(status.last_verified_at, None);

    let setup = guarded("setup_status", {
        let worker = worker.clone();
        move || {
            worker
                .setup("status", serde_json::json!({}))
                .map_err(|e| e.to_string())
        }
    });
    assert_eq!(setup["stage"], "source");
    assert_eq!(setup["has_credentials"], false);
    assert!(setup.get("credential").is_none());

    let preview = guarded("preview_fixture", {
        let worker = worker.clone();
        move || {
            worker
                .preview_fixture("2026-09-14", "2026-09-20")
                .map_err(|e| e.to_string())
        }
    });
    assert!(!preview.items.is_empty());
    assert!(!preview.digest.is_empty());
    assert_eq!(
        preview.counts.values().sum::<u64>(),
        preview.items.len() as u64
    );
}
