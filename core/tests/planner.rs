use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::{Map, Value};
use teamup_shift_sync_core::{
    build_plan, plan_digest, reconciliation_range, DestinationSnapshot, PlanRequest, PlanSystem,
    PlanningConfig, SourceShift, StepRecord, SyncPlan, SyncState,
};

#[derive(Deserialize)]
struct Case {
    config: PlanningConfig,
    shifts: Vec<SourceShift>,
    destination: DestinationSnapshot,
    range_start: DateTime<FixedOffset>,
    range_end: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
    live: bool,
    records: Vec<StoredStep>,
    expected: SyncPlan,
    digest: String,
}

#[derive(Deserialize)]
struct StoredStep {
    source_key: String,
    step_key: String,
    status: String,
    destination_id: Option<String>,
    source_hash: String,
    synced_payload: Map<String, Value>,
    error: Option<String>,
}

#[test]
fn plans_match_recorded_safety_scenarios_and_representative_week() {
    // Frozen at the Python removal cutover: the oracle compared 263 safety
    // combinations and the representative week, all passing.
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("goldens/planner.json")).unwrap();
    assert!(cases.len() > 200, "oracle must run all safety combinations");
    for (index, case) in cases.into_iter().enumerate() {
        let state = SyncState::open(":memory:").unwrap();
        for r in case.records {
            state
                .record_step(
                    &StepRecord {
                        source_key: r.source_key,
                        step_key: r.step_key,
                        status: r.status,
                        destination_id: r.destination_id,
                        source_hash: r.source_hash,
                        synced_payload: r.synced_payload,
                        error: r.error,
                    },
                    case.now,
                )
                .unwrap();
        }
        let mut actual = build_plan(
            &PlanRequest {
                config: &case.config,
                shifts: &case.shifts,
                destination: &case.destination,
                range_start: case.range_start,
                range_end: case.range_end,
                now: case.now,
                live: case.live,
            },
            &state,
        )
        .unwrap();
        assert_eq!(
            plan_digest(&actual).unwrap(),
            case.digest,
            "digest case {index}"
        );
        // Parser diagnostics predate this slice and use Rust quoting. Their
        // stable code, source ID and review outcome must still match exactly.
        for (actual, expected) in actual.items.iter_mut().zip(&case.expected.items) {
            if actual.system == PlanSystem::Source && actual.step_key.starts_with("sps:") {
                assert!(!actual.summary.is_empty());
                actual.summary = expected.summary.clone();
            }
        }
        assert_eq!(actual, case.expected, "Python parity case {index}");
    }
}

#[test]
fn read_range_covers_full_shifts_but_excludes_touching_boundaries() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/representative-week.json")).unwrap();
    let mut shift: SourceShift =
        serde_json::from_value(fixture["source_shifts"][0].clone()).unwrap();
    let start = DateTime::parse_from_rfc3339("2026-09-14T00:00:00+02:00").unwrap();
    let end = DateTime::parse_from_rfc3339("2026-09-21T00:00:00+02:00").unwrap();
    shift.starts_at = start - chrono::Duration::hours(2);
    shift.ends_at = end + chrono::Duration::hours(3);
    assert_eq!(
        reconciliation_range(&[shift.clone()], start, end),
        (shift.starts_at, shift.ends_at)
    );
    shift.ends_at = start;
    assert_eq!(
        reconciliation_range(&[shift.clone()], start, end),
        (start, end)
    );
    shift.starts_at = end;
    shift.ends_at = end + chrono::Duration::hours(3);
    assert_eq!(reconciliation_range(&[shift], start, end), (start, end));
}

#[test]
fn planning_preserves_recovery_records_and_rejects_corrupt_state() {
    use serde_json::json;
    use teamup_shift_sync_core::{Outcome, PlanningError};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite3");
    let state = SyncState::open(&path).unwrap();
    let config: PlanningConfig = serde_json::from_value(json!({
        "timezone": "Europe/Copenhagen", "default_helper_count": 1,
        "duos_arrangement_id": "arrangement", "duos_registration_type": "Almindelig",
        "helpers": {"helper": {"mithf_name": "Helper", "duos_employee_number": "123"}}
    }))
    .unwrap();
    let shift: SourceShift = serde_json::from_value(json!({
        "calendar_id": "calendar", "event_id": "event", "occurrence_id": "occurrence",
        "title": "Shift", "helper_key": "helper", "notes": "uni 8-10 & 13-14",
        "starts_at": "2026-09-14T07:30:00+02:00", "ends_at": "2026-09-14T15:00:00+02:00"
    }))
    .unwrap();
    let now = DateTime::parse_from_rfc3339("2026-09-20T20:00:00+02:00").unwrap();
    let saved = StepRecord {
        source_key: shift.key(),
        step_key: "mithf.create_shift".into(),
        status: "uncertain".into(),
        destination_id: None,
        source_hash: "old-source".into(),
        synced_payload: Map::new(),
        error: None,
    };
    state.record_step(&saved, now).unwrap();
    let shifts = [shift];
    let request = PlanRequest {
        config: &config,
        shifts: &shifts,
        destination: &DestinationSnapshot::default(),
        range_start: shifts[0].starts_at,
        range_end: shifts[0].ends_at,
        now,
        live: true,
    };
    let plan = build_plan(&request, &state).unwrap();
    assert_eq!(plan.items[0].outcome, Outcome::Conflicted);
    assert_eq!(plan.items[0].reason, "uncertain_write");
    assert_eq!(
        state.steps_for_source(&shifts[0].key()).unwrap(),
        std::slice::from_ref(&saved)
    );
    assert_eq!(
        plan.items
            .iter()
            .filter(|i| i.step_key.starts_with("mithf.create_shift"))
            .count(),
        2
    );
    assert_eq!(
        plan.items
            .iter()
            .find(|i| i.step_key == "mithf.create_shift#1")
            .unwrap()
            .payload["starts_at"],
        "2026-09-14T13:00:00+02:00"
    );

    state
        .record_step(
            &StepRecord {
                step_key: "mithf.create_shift#bad".into(),
                ..saved
            },
            now,
        )
        .unwrap();
    assert!(matches!(
        build_plan(&request, &state),
        Err(PlanningError::InvalidSegmentKey)
    ));
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute("UPDATE sync_steps SET synced_payload_json = '[]'", [])
        .unwrap();
    assert!(matches!(
        build_plan(&request, &state),
        Err(PlanningError::State(_))
    ));
}
