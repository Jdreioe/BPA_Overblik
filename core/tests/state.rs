use std::{path::Path, sync::mpsc};

use chrono::{DateTime, FixedOffset};
use rusqlite::Connection;
use serde_json::json;
use teamup_shift_sync_core::{SourceShift, StateError, StepRecord, SyncState};
use tempfile::tempdir;

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-09-20T20:00:00.120000+02:00").unwrap()
}

fn record() -> StepRecord {
    StepRecord {
        source_key: "rust-source".into(),
        step_key: "duos.register".into(),
        status: "uncertain".into(),
        destination_id: None,
        source_hash: "rust-hash".into(),
        synced_payload: json!({"name": "æøå", "intervals": [1, 2]})
            .as_object()
            .unwrap()
            .clone(),
        error: None,
    }
}

// Compare all persisted values, including hashes, timestamps, and retained comments.
fn snapshot_rows(path: &Path) -> Vec<Vec<String>> {
    let connection = Connection::open(path).unwrap();
    ["source_occurrences", "source_comments"]
        .into_iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
                .unwrap();
            let count = statement.column_count();
            statement
                .query_map([], |row| {
                    Ok((0..count)
                        .map(|i| row.get::<_, Option<String>>(i).unwrap())
                        .collect::<Vec<_>>())
                })
                .unwrap()
                .map(|row| serde_json::to_string(&row.unwrap()).unwrap())
                .collect()
        })
        .collect()
}

#[test]
fn legacy_python_database_copy_preserves_recovery_and_snapshot_hashes() {
    // Database file written by the Python engine before its removal, frozen
    // at the cutover along with the shifts it stored.
    let directory = tempdir().unwrap();
    let original = directory.path().join("python.sqlite3");
    std::fs::write(
        &original,
        include_bytes!("goldens/python-created.sqlite3"),
    )
    .unwrap();
    let shifts: Vec<SourceShift> =
        serde_json::from_str(include_str!("goldens/python-created-shifts.json")).unwrap();
    let copy = directory.path().join("copy.sqlite3");
    std::fs::copy(&original, &copy).unwrap();
    let before = snapshot_rows(&copy);
    let mut state = SyncState::open(&copy).unwrap();
    assert_eq!(before, snapshot_rows(&copy));
    for (index, shift) in shifts.iter().enumerate() {
        let step = state
            .get_step(&shift.key(), "mithf.create_shift")
            .unwrap()
            .unwrap();
        assert_eq!(
            step.status,
            if index == 0 { "uncertain" } else { "verified" }
        );
        assert_eq!(
            step.destination_id,
            if index == 0 {
                None
            } else {
                Some(index.to_string())
            }
        );
        assert_eq!(
            step.error,
            if index == 0 {
                Some("read-back interrupted".into())
            } else {
                None
            }
        );
        assert_eq!(step.source_hash, "existing-hash");
        assert_eq!(
            step.synced_payload,
            json!({"helper": "æøå", "count": 1})
                .as_object()
                .unwrap()
                .clone()
        );
        state.record_source_snapshot(shift, now()).unwrap();
    }
    assert_eq!(
        before,
        snapshot_rows(&copy),
        "Rust must preserve Python's snapshot values exactly"
    );
    state.record_step(&record(), now()).unwrap();
    // The removed Python check read this record back field by field. The
    // same values are asserted in Rust now.
    let stored = SyncState::open(&copy)
        .unwrap()
        .get_step("rust-source", "duos.register")
        .unwrap()
        .unwrap();
    assert_eq!(stored, record());
    assert!(SyncState::open(&original)
        .unwrap()
        .get_step("rust-source", "duos.register")
        .unwrap()
        .is_none());
}

#[test]
fn upgrades_legacy_schema_without_losing_history_and_keeps_accounts_separate() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("sync-account-a.sqlite3");
    let schema = include_str!("../src/state_schema.sql")
        // Git may check out the schema with CRLF on Windows.
        .replace("\r\n", "\n")
        .replace("    recurrence_start TEXT,\n", "")
        .replace("    source_version TEXT,\n", "");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(&schema).unwrap();
    connection.execute("INSERT INTO source_occurrences VALUES ('old', 'calendar', 'event', 'occurrence', 'old-hash', 'old-time')", []).unwrap();
    drop(connection);
    let state = SyncState::open(&path).unwrap();
    state.record_step(&record(), now()).unwrap();
    drop(state);
    let mut reopened = SyncState::open(&path).unwrap();
    assert_eq!(
        reopened.get_step("rust-source", "duos.register").unwrap(),
        Some(record())
    );
    let mut verified = record();
    verified.status = "verified".into();
    verified.destination_id = Some("confirmed-id".into());
    reopened.record_step(&verified, now()).unwrap();
    assert_eq!(
        reopened.steps_for_source("rust-source").unwrap(),
        [verified]
    );
    let connection = Connection::open(&path).unwrap();
    let row = connection.query_row("SELECT snapshot_hash, recurrence_start, source_version FROM source_occurrences WHERE source_key = 'old'", [], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
    }).unwrap();
    assert_eq!(row, ("old-hash".into(), None, None));
    let other = SyncState::open(directory.path().join("sync-account-b.sqlite3")).unwrap();
    assert!(other.steps_for_source("rust-source").unwrap().is_empty());
    let mut second = record();
    second.source_key = "other-source".into();
    reopened.record_step(&second, now()).unwrap();
    assert_eq!(
        reopened.forget_steps("rust-source").unwrap(),
        ["duos.register"]
    );
    assert!(reopened.forget_steps("rust-source").unwrap().is_empty());
    assert_eq!(reopened.steps_for_source("other-source").unwrap(), [second]);
}

#[test]
fn corrupt_payloads_fail_closed_instead_of_looking_unsynchronized() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.sqlite3");
    let state = SyncState::open(&path).unwrap();
    state.record_step(&record(), now()).unwrap();
    let connection = Connection::open(&path).unwrap();
    for payload in ["not json", "null", "[]"] {
        connection
            .execute("UPDATE sync_steps SET synced_payload_json = ?", [payload])
            .unwrap();
        assert!(matches!(
            state.get_step("rust-source", "duos.register"),
            Err(StateError::Payload(_))
        ));
        assert!(matches!(
            state.steps_for_source("rust-source"),
            Err(StateError::Payload(_))
        ));
    }
}

#[test]
fn apply_lock_excludes_concurrent_holders_and_releases_on_unwind() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.sqlite3");
    let state = SyncState::open(&path).unwrap();
    let other = SyncState::open(&path).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.exclusive_apply().unwrap();
        assert!(matches!(
            other.exclusive_apply(),
            Err(StateError::ApplyInProgress)
        ));
        // A second holder on its own handle blocks as well. It runs on a
        // thread so the lock file stays held while this side waits.
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = std::thread::spawn({
            let path = path.clone();
            move || {
                let held = SyncState::open(&path).unwrap();
                let _guard = held.exclusive_apply().unwrap();
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
        });
        drop(_guard);
        ready_rx.recv().unwrap();
        assert!(matches!(
            state.exclusive_apply(),
            Err(StateError::ApplyInProgress)
        ));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        // Panic while holding the lock: unwinding must release it.
        let _guard = state.exclusive_apply().unwrap();
        panic!("simulated apply interruption");
    }));
    assert!(result.is_err());
    let _guard = state.exclusive_apply().unwrap();
}

#[test]
fn failed_snapshot_rolls_back_occurrence_and_comment_updates() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.sqlite3");
    let mut state = SyncState::open(&path).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/representative-week.json")).unwrap();
    let mut shift: SourceShift =
        serde_json::from_value(fixture["source_shifts"][0].clone()).unwrap();
    state.record_source_snapshot(&shift, now()).unwrap();
    let before = snapshot_rows(&path);
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_comment BEFORE UPDATE ON source_comments BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    shift.title = "changed".into();
    shift.comments[0].text = "changed".into();
    assert!(state.record_source_snapshot(&shift, now()).is_err());
    assert_eq!(snapshot_rows(&path), before);
}
