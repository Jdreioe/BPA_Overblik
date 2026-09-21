use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
use teamup_shift_sync_core::{StepRecord, SyncState};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .into()
}
fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_teamup-shift-sync-rust"));
    command.current_dir(root());
    command
}
fn preview(state: &Path, fixture: &Path, extra: &[&str]) -> Output {
    cli()
        .args([
            "dry-run",
            "--config",
            "fixtures/offline-config.toml",
            "--fixture",
        ])
        .arg(fixture)
        .arg("--state")
        .arg(state)
        .args(["--now", "2026-09-20T20:00:00+02:00", "--json"])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn representative_cli_preview_matches_python_and_preserves_state() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.sqlite3");
    let output = preview(
        &state_path,
        Path::new("fixtures/representative-week.json"),
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual["mode"], "fixture");
    assert_eq!(actual["has_blockers"], true);
    // Frozen at the Python removal cutover: same digest and outcomes as the
    // Python planner on the representative week.
    let expected: Value =
        serde_json::from_str(include_str!("goldens/representative-preview.json")).unwrap();
    assert_eq!(actual["digest"], expected["digest"]);
    let items = actual["plan"]["items"].as_array().unwrap();
    assert_eq!(
        Value::Array(items.iter().map(|i| i["outcome"].clone()).collect()),
        expected["outcomes"]
    );
    let state = SyncState::open(&state_path).unwrap();
    for item in items {
        assert!(state
            .steps_for_source(item["source_key"].as_str().unwrap())
            .unwrap()
            .is_empty());
    }
    // Repeating a read must produce the same approval hash.
    let repeated = preview(
        &state_path,
        Path::new("fixtures/representative-week.json"),
        &[],
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&repeated.stdout).unwrap()["digest"],
        actual["digest"]
    );
}

#[test]
fn empty_weeks_and_dst_use_local_midnights() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("empty.json");
    std::fs::write(
        &fixture,
        r#"{"source_shifts":[],"mithf_shifts":[],"duos_registrations":[]}"#,
    )
    .unwrap();
    for (from, to, hours) in [
        ("2026-03-23", "2026-03-29", 167),
        ("2026-10-19", "2026-10-25", 169),
    ] {
        let result = preview(
            &dir.path().join("state.sqlite3"),
            &fixture,
            &["--from", from, "--to", to],
        );
        assert!(result.status.success());
        let result: Value = serde_json::from_slice(&result.stdout).unwrap();
        let start =
            chrono::DateTime::parse_from_rfc3339(result["plan"]["starts_at"].as_str().unwrap())
                .unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(result["plan"]["ends_at"].as_str().unwrap())
            .unwrap();
        assert_eq!((end - start).num_hours(), hours);
        assert_eq!(result["has_blockers"], false);
    }
}

#[test]
fn invalid_input_fails_without_creating_state_or_disclosing_contents() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("bad.json");
    let state = dir.path().join("must-not-exist.sqlite3");
    std::fs::write(
        &fixture,
        r#"{"source_shifts":[],"private":"PRIVATE-CONTENT"}"#,
    )
    .unwrap();
    let result = preview(&state, &fixture, &[]);
    assert_eq!(result.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("PRIVATE-CONTENT"));
    assert!(!state.exists());
    let result = preview(
        &state,
        Path::new("fixtures/representative-week.json"),
        &["--from", "2026-09-21", "--to", "2026-09-14"],
    );
    assert_eq!(result.status.code(), Some(2));
    assert!(!state.exists());
}

#[test]
fn apply_requires_explicit_dates_and_digest_and_rejects_fixture_controls() {
    let digest = "a".repeat(64);
    for args in [
        vec![
            "apply",
            "--data-dir",
            "absent",
            "--from",
            "2026-09-14",
            "--to",
            "2026-09-20",
        ],
        vec!["apply", "--data-dir", "absent", "--approve", &digest],
        vec![
            "apply",
            "--data-dir",
            "absent",
            "--from",
            "2026-09-14",
            "--to",
            "2026-09-20",
            "--approve",
            "bad",
        ],
        vec![
            "apply",
            "--data-dir",
            "absent",
            "--from",
            "2026-09-14",
            "--to",
            "2026-09-20",
            "--approve",
            &digest,
            "--fixture",
            "fixture.json",
        ],
        vec![
            "dry-run",
            "--live",
            "--data-dir",
            "absent",
            "--now",
            "2026-09-20T20:00:00+02:00",
        ],
        vec![
            "dry-run",
            "--live",
            "--data-dir",
            "absent",
            "--state",
            "other.sqlite3",
        ],
    ] {
        let result = cli().args(args).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("--help"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn forgetting_is_scoped_and_respects_the_apply_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    let state = SyncState::open(&path).unwrap();
    for source in ["selected", "other"] {
        state
            .record_step(
                &StepRecord {
                    source_key: source.into(),
                    step_key: "mithf.create_shift".into(),
                    status: "uncertain".into(),
                    destination_id: None,
                    source_hash: "hash".into(),
                    synced_payload: json!({}).as_object().unwrap().clone(),
                    error: None,
                },
                chrono::Utc::now().fixed_offset(),
            )
            .unwrap();
    }
    let forget = || {
        cli()
            .arg("forget")
            .arg("--state")
            .arg(&path)
            .args(["--shift", "selected", "selected"])
            .output()
            .unwrap()
    };
    let guard = state.exclusive_apply().unwrap();
    assert_eq!(forget().status.code(), Some(2));
    assert_eq!(state.steps_for_source("selected").unwrap().len(), 1);
    drop(guard);
    assert!(forget().status.success());
    assert!(state.steps_for_source("selected").unwrap().is_empty());
    assert_eq!(state.steps_for_source("other").unwrap().len(), 1);
    assert_eq!(forget().status.code(), Some(1));
}
