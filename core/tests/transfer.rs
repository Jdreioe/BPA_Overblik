use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::{json, Value};
use teamup_shift_sync_core::{
    apply_plan, apply_plan_controlled, build_plan, plan_digest, ApplyOutcome, ApplyRequest,
    ApprovalError, DestinationSnapshot, Destinations, DuosRegistration, MitHfShift, Outcome,
    PlanItem, PlanRequest, PlanningConfig, SourceShift, StateError, StepRecord, SyncState,
    TimeInterval, TransferError, TransferEvent,
};

#[derive(Default)]
struct MemoryDestinations {
    snapshot: DestinationSnapshot,
    writes: Vec<String>,
    timeout_after_create: bool,
    hang_on_write: bool,
    write_started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fail_read: Option<usize>,
    ignore_write: bool,
    reads: usize,
    state_path: PathBuf,
    ranges: Vec<(DateTime<FixedOffset>, DateTime<FixedOffset>)>,
}

impl Destinations for MemoryDestinations {
    type Error = io::Error;

    async fn read(
        &mut self,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<DestinationSnapshot, Self::Error> {
        self.reads += 1;
        self.ranges.push((start, end));
        let observer = SyncState::open(&self.state_path).unwrap();
        assert!(
            matches!(observer.exclusive_apply(), Err(StateError::ApplyInProgress)),
            "apply lock must cover every read"
        );
        if self.fail_read == Some(self.reads) {
            return Err(io::Error::other("Incomplete destination read"));
        }
        Ok(self.snapshot.clone())
    }

    async fn write(
        &mut self,
        item: &PlanItem,
        snapshot: &DestinationSnapshot,
    ) -> Result<Option<String>, Self::Error> {
        // A different SQLite connection must see the committed marker BEFORE
        // any remote mutation, and must be excluded by the apply lock.
        let observer = SyncState::open(&self.state_path).unwrap();
        let record = observer
            .get_step(&item.source_key, &item.step_key)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, "uncertain");
        assert_eq!(record.synced_payload, item.payload);
        assert_eq!(record.destination_id, item.destination_id);
        assert!(matches!(
            observer.exclusive_apply(),
            Err(StateError::ApplyInProgress)
        ));
        self.writes.push(item.step_key.clone());
        self.write_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if self.hang_on_write {
            return std::future::pending().await;
        }
        if self.ignore_write {
            return Ok(None);
        }
        let base = item.step_key.split('#').next().unwrap();
        let payload = &item.payload;
        let mut shifts = snapshot.mithf_shifts.clone();
        let result = match base {
            "mithf.create_shift" => {
                let id = item
                    .destination_id
                    .clone()
                    .unwrap_or_else(|| format!("new-{}", shifts.len()));
                if let Some(shift) = shifts.iter_mut().find(|s| s.id == id) {
                    shift.starts_at = moment(payload["starts_at"].as_str().unwrap());
                    shift.ends_at = moment(payload["ends_at"].as_str().unwrap());
                    shift.helper_count = payload["helper_count"].as_i64().unwrap();
                } else {
                    shifts.push(MitHfShift {
                        id: id.clone(),
                        starts_at: moment(payload["starts_at"].as_str().unwrap()),
                        ends_at: moment(payload["ends_at"].as_str().unwrap()),
                        helper_count: payload["helper_count"].as_i64().unwrap(),
                        helper_name: None,
                        sps_intervals: vec![],
                        meeting_intervals: vec![],
                        sps_record_ids: vec![],
                        meeting_record_ids: vec![],
                    });
                }
                Some(id)
            }
            "mithf.assign_helper" => {
                let shift = shifts
                    .iter_mut()
                    .find(|s| Some(&s.id) == item.destination_id.as_ref())
                    .unwrap();
                shift.id = format!("booked-{}", shift.id);
                shift.helper_name = Some(payload["helper_name"].as_str().unwrap().into());
                Some(shift.id.clone())
            }
            "mithf.set_sps" => {
                let shift = shifts
                    .iter_mut()
                    .find(|s| Some(&s.id) == item.destination_id.as_ref())
                    .unwrap();
                shift.sps_intervals = serde_json::from_value(payload["intervals"].clone()).unwrap();
                Some(shift.id.clone())
            }
            "mithf.set_meeting" => {
                let shift = shifts
                    .iter_mut()
                    .find(|s| Some(&s.id) == item.destination_id.as_ref())
                    .unwrap();
                shift.meeting_intervals = vec![TimeInterval {
                    starts_at: moment(payload["starts_at"].as_str().unwrap()),
                    ends_at: moment(payload["ends_at"].as_str().unwrap()),
                }];
                Some(shift.id.clone())
            }
            _ if item.step_key.starts_with("duos.interval:") => {
                let registration: DuosRegistration = serde_json::from_value(json!({
                    "id": item.destination_id.clone().unwrap_or_else(|| format!("duos-{}", self.snapshot.duos_registrations.len())),
                    "arrangement_id": payload["arrangement_id"], "employee_number": payload["employee_number"],
                    "registration_type": payload["registration_type"], "starts_at": payload["starts_at"], "ends_at": payload["ends_at"],
                })).unwrap();
                self.snapshot
                    .duos_registrations
                    .retain(|r| Some(&r.id) != item.destination_id.as_ref());
                self.snapshot.duos_registrations.push(registration);
                None
            }
            _ => panic!("unexpected test operation"),
        };
        shifts.sort_by_key(|s| s.starts_at);
        self.snapshot.mithf_shifts = shifts;
        if base == "mithf.create_shift" && self.timeout_after_create {
            self.timeout_after_create = false;
            return Err(io::Error::other("Response lost"));
        }
        Ok(result)
    }
}

fn moment(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn request() -> ApplyRequest {
    ApplyRequest {
        config: serde_json::from_value(
            json!({"timezone": "Europe/Copenhagen", "default_helper_count": 1,
            "duos_arrangement_id": "arrangement", "duos_registration_type": "Almindelig",
            "helpers": {"helper": {"mithf_name": "Mit Helper", "duos_employee_number": "123"}}}),
        )
        .unwrap(),
        shifts: vec![serde_json::from_value(
            json!({"calendar_id": "calendar", "event_id": "event", "occurrence_id": "occurrence",
            "title": "Shift", "helper_key": "helper", "notes": "uni 8-10 & 13-14",
            "starts_at": "2026-09-14T07:30:00+02:00", "ends_at": "2026-09-14T15:00:00+02:00"}),
        )
        .unwrap()],
        range_start: moment("2026-09-14T00:00:00+02:00"),
        range_end: moment("2026-09-21T00:00:00+02:00"),
        now: moment("2026-09-20T20:00:00+02:00"),
        expected_digest: String::new(),
    }
}

fn preview_plan(
    request: &ApplyRequest,
    adapter: &MemoryDestinations,
) -> teamup_shift_sync_core::SyncPlan {
    let state = SyncState::open(&adapter.state_path).unwrap();
    build_plan(
        &PlanRequest {
            config: &request.config,
            shifts: &request.shifts,
            destination: &adapter.snapshot,
            range_start: request.range_start,
            range_end: request.range_end,
            now: request.now,
            live: true,
        },
        &state,
    )
    .unwrap()
}

fn approve(request: &mut ApplyRequest, adapter: &MemoryDestinations) {
    request.expected_digest = plan_digest(&preview_plan(request, adapter)).unwrap();
}

fn snapshots(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM source_occurrences", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn changed_shift_times_are_approved_and_verified_for_day_and_overnight_shifts() {
    for (old_start, old_end, new_start, new_end) in [
        (
            "2026-09-14T07:30:00+02:00",
            "2026-09-14T15:00:00+02:00",
            "2026-09-14T08:30:00+02:00",
            "2026-09-14T16:00:00+02:00",
        ),
        (
            "2026-09-14T22:00:00+02:00",
            "2026-09-15T06:00:00+02:00",
            "2026-09-14T23:00:00+02:00",
            "2026-09-15T07:00:00+02:00",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut adapter = MemoryDestinations {
            state_path: temp.path().join("sync.sqlite3"),
            ..Default::default()
        };
        let mut request = request();
        request.shifts[0].notes.clear();
        request.shifts[0].starts_at = moment(old_start);
        request.shifts[0].ends_at = moment(old_end);
        approve(&mut request, &adapter);
        runtime()
            .block_on(apply_plan(
                request.clone(),
                adapter.state_path.clone(),
                &mut adapter,
                |_| {},
            ))
            .unwrap();
        let original = adapter.snapshot.mithf_shifts[0].clone();
        adapter.writes.clear();

        request.shifts[0].starts_at = moment(new_start);
        request.shifts[0].ends_at = moment(new_end);
        let preview = preview_plan(&request, &adapter);
        let edit = preview
            .items
            .iter()
            .find(|item| item.step_key == "mithf.create_shift")
            .unwrap();
        assert_eq!(edit.outcome, Outcome::WouldUpdate);
        assert_eq!(edit.destination_id.as_deref(), Some(original.id.as_str()));
        approve(&mut request, &adapter);
        let verified = runtime()
            .block_on(apply_plan(
                request,
                adapter.state_path.clone(),
                &mut adapter,
                |_| {},
            ))
            .unwrap();
        assert_eq!(adapter.writes, ["mithf.create_shift"]);
        assert!(verified
            .items
            .iter()
            .all(|item| item.outcome == Outcome::AlreadyMatched));
        let updated = &adapter.snapshot.mithf_shifts[0];
        assert_eq!(
            (updated.starts_at, updated.ends_at),
            (moment(new_start), moment(new_end))
        );
        assert_eq!(updated.id, original.id);
        assert_eq!(updated.helper_name, original.helper_name);
    }
}

#[test]
fn split_sps_edit_updates_only_its_own_mithf_part() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    for shift in &mut adapter.snapshot.mithf_shifts {
        shift.sps_record_ids = vec![format!("rid-{}", shift.id)];
    }
    let first = adapter.snapshot.mithf_shifts[0].clone();
    let second = adapter.snapshot.mithf_shifts[1].clone();
    adapter.writes.clear();

    request.shifts[0].notes = "uni 8-10 & 13-15".into();
    let preview = preview_plan(&request, &adapter);
    let first_sps = preview
        .items
        .iter()
        .find(|i| i.step_key == "mithf.set_sps")
        .unwrap();
    let second_sps = preview
        .items
        .iter()
        .find(|i| i.step_key == "mithf.set_sps#1")
        .unwrap();
    assert_eq!(first_sps.outcome, Outcome::AlreadyMatched);
    assert_eq!(second_sps.outcome, Outcome::WouldUpdate);
    assert_eq!(
        second_sps.destination_id.as_deref(),
        Some(second.id.as_str())
    );
    approve(&mut request, &adapter);
    let verified = runtime()
        .block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    assert!(verified
        .items
        .iter()
        .all(|i| i.outcome == Outcome::AlreadyMatched));
    assert_eq!(adapter.writes, ["mithf.set_sps#1", "duos.interval:notes:1"]);
    assert_eq!(adapter.snapshot.mithf_shifts[0], first);
    let updated = &adapter.snapshot.mithf_shifts[1];
    assert_eq!(
        (updated.starts_at, updated.ends_at),
        (second.starts_at, second.ends_at)
    );
    assert_eq!(
        updated.sps_intervals[0].ends_at,
        moment("2026-09-14T15:00:00+02:00")
    );
    assert_eq!(updated.sps_record_ids, second.sps_record_ids);

    adapter.writes.clear();
    adapter.snapshot.mithf_shifts[1].sps_intervals[0].ends_at = moment("2026-09-14T14:30:00+02:00");
    request.shifts[0].notes = "uni 8-10 & 13-14".into();
    let restored = preview_plan(&request, &adapter);
    let sps = restored
        .items
        .iter()
        .find(|i| i.step_key == "mithf.set_sps#1")
        .unwrap();
    assert_eq!(sps.outcome, Outcome::WouldUpdate);
    assert_eq!(
        sps.payload["intervals"][0]["ends_at"],
        "2026-09-14T14:00:00+02:00"
    );
}

#[test]
fn an_earlier_sps_interval_splits_a_transferred_shift_without_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    request.shifts[0].notes = "uni 13-14".into();
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    let original = adapter.snapshot.mithf_shifts[0].clone();
    let registration = adapter.snapshot.duos_registrations[0].id.clone();
    adapter.writes.clear();

    // The new first part moves the existing shift and registration; the
    // second part overlaps them only until those updates are written.
    request.shifts[0].notes = "uni 8-10 & 13-14".into();
    let preview = preview_plan(&request, &adapter);
    let outcome = |key: &str| {
        preview
            .items
            .iter()
            .find(|i| i.step_key == key)
            .unwrap()
            .outcome
    };
    assert_eq!(outcome("mithf.create_shift"), Outcome::WouldUpdate);
    assert_eq!(outcome("mithf.create_shift#1"), Outcome::WouldCreate);
    assert_eq!(outcome("duos.interval:notes:0"), Outcome::WouldUpdate);
    assert_eq!(outcome("duos.interval:notes:1"), Outcome::WouldCreate);
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request,
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();

    let shifts = &adapter.snapshot.mithf_shifts;
    assert_eq!(shifts.len(), 2);
    assert_eq!(shifts[0].id, original.id);
    let bounds: Vec<_> = shifts.iter().map(|s| (s.starts_at, s.ends_at)).collect();
    assert_eq!(
        bounds,
        [
            (original.starts_at, moment("2026-09-14T13:00:00+02:00")),
            (moment("2026-09-14T13:00:00+02:00"), original.ends_at),
        ]
    );
    let registrations = &adapter.snapshot.duos_registrations;
    assert_eq!(registrations.len(), 2);
    assert!(registrations
        .iter()
        .any(|r| r.id == registration && r.starts_at == moment("2026-09-14T08:00:00+02:00")));
}

#[test]
fn manual_mithf_changes_are_changed_back_to_the_shift_plan() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    request.shifts[0].notes.clear();
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    adapter.writes.clear();
    adapter.snapshot.mithf_shifts[0].ends_at = moment("2026-09-14T14:30:00+02:00");
    request.shifts[0].ends_at = moment("2026-09-14T16:00:00+02:00");
    let preview = preview_plan(&request, &adapter);
    let edit = preview
        .items
        .iter()
        .find(|i| i.step_key == "mithf.create_shift")
        .unwrap();
    assert_eq!(edit.outcome, Outcome::WouldUpdate);
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request,
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    assert_eq!(adapter.writes, ["mithf.create_shift"]);
    assert_eq!(
        adapter.snapshot.mithf_shifts[0].ends_at,
        moment("2026-09-14T16:00:00+02:00")
    );
}

#[test]
fn a_hand_entered_shift_is_shortened_before_another_helper_takes_its_hours() {
    // A's Friday-Sunday shift was entered in MitHF by hand. B takes over the
    // end of it, or the start; either way A's shift changes first.
    for (a, b) in [
        (
            ("2026-09-18T16:00:00+02:00", "2026-09-20T17:30:00+02:00"),
            ("2026-09-20T17:30:00+02:00", "2026-09-20T20:00:00+02:00"),
        ),
        (
            ("2026-09-18T18:00:00+02:00", "2026-09-20T20:00:00+02:00"),
            ("2026-09-18T16:00:00+02:00", "2026-09-18T18:00:00+02:00"),
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut adapter = MemoryDestinations {
            state_path: temp.path().join("sync.sqlite3"),
            ..Default::default()
        };
        adapter.snapshot.mithf_shifts.push(MitHfShift {
            id: "manual".into(),
            starts_at: moment("2026-09-18T16:00:00+02:00"),
            ends_at: moment("2026-09-20T20:00:00+02:00"),
            helper_count: 1,
            helper_name: Some("Mit A".into()),
            sps_intervals: vec![],
            meeting_intervals: vec![],
            sps_record_ids: vec![],
            meeting_record_ids: vec![],
        });
        let mut request = request();
        request.config = serde_json::from_value(json!({
            "timezone": "Europe/Copenhagen", "default_helper_count": 1,
            "duos_arrangement_id": "arrangement", "duos_registration_type": "Almindelig",
            "helpers": {"a": {"mithf_name": "Mit A", "duos_employee_number": "1"},
                        "b": {"mithf_name": "Mit B", "duos_employee_number": "2"}}}))
        .unwrap();
        request.shifts = [("a", a), ("b", b)]
            .map(|(helper, (starts_at, ends_at))| {
                serde_json::from_value(json!({
                    "calendar_id": "calendar", "event_id": helper, "occurrence_id": helper,
                    "title": "Shift", "helper_key": helper,
                    "starts_at": starts_at, "ends_at": ends_at}))
                .unwrap()
            })
            .into();

        let preview = preview_plan(&request, &adapter);
        let outcomes: Vec<_> = preview
            .items
            .iter()
            .map(|i| (i.source_key.as_str(), i.step_key.as_str(), i.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("calendar:a:a", "mithf.create_shift", Outcome::WouldUpdate),
                (
                    "calendar:a:a",
                    "mithf.assign_helper",
                    Outcome::AlreadyMatched
                ),
                ("calendar:b:b", "mithf.create_shift", Outcome::WouldCreate),
                ("calendar:b:b", "mithf.assign_helper", Outcome::WouldCreate),
            ]
        );
        assert_eq!(preview.items[0].destination_id.as_deref(), Some("manual"));
        approve(&mut request, &adapter);
        runtime()
            .block_on(apply_plan(
                request,
                adapter.state_path.clone(),
                &mut adapter,
                |_| {},
            ))
            .unwrap();
        assert_eq!(
            adapter.writes,
            [
                "mithf.create_shift",
                "mithf.create_shift",
                "mithf.assign_helper"
            ]
        );
        let shift = |name: &str| {
            let s = adapter
                .snapshot
                .mithf_shifts
                .iter()
                .find(|s| s.helper_name.as_deref() == Some(name))
                .unwrap();
            (s.starts_at, s.ends_at)
        };
        assert_eq!(shift("Mit A"), (moment(a.0), moment(a.1)));
        assert_eq!(shift("Mit B"), (moment(b.0), moment(b.1)));
    }
}

#[test]
fn split_transfers_verify_changed_ids_and_repeat_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    let mut progress = Vec::new();
    let final_plan = runtime()
        .block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |event| {
                if let teamup_shift_sync_core::TransferEvent::Verified(operation) = event {
                    progress.push(operation.step_key);
                }
            },
        ))
        .unwrap();
    assert!(final_plan
        .items
        .iter()
        .all(|i| i.outcome == Outcome::AlreadyMatched));
    assert_eq!(adapter.snapshot.mithf_shifts.len(), 2);
    assert_eq!(adapter.snapshot.duos_registrations.len(), 2);
    assert_eq!(adapter.writes.len(), 8);
    assert_eq!(progress, adapter.writes);
    assert_eq!(snapshots(&adapter.state_path), 1);
    let state = SyncState::open(&adapter.state_path).unwrap();
    for (key, id) in [
        ("mithf.create_shift", "booked-new-0"),
        ("mithf.create_shift#1", "booked-new-1"),
    ] {
        let record = state
            .get_step(&request.shifts[0].key(), key)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, "verified");
        assert_eq!(record.destination_id.as_deref(), Some(id));
    }
    approve(&mut request, &adapter);
    adapter.writes.clear();
    runtime()
        .block_on(apply_plan(
            request,
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    assert!(adapter.writes.is_empty());
}

#[test]
fn requested_stop_finishes_readback_and_stops_before_the_next_write() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    let verified = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let progress_count = verified.clone();
    let stop_count = verified.clone();

    let outcome = runtime()
        .block_on(apply_plan_controlled(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            move |event| {
                if matches!(event, TransferEvent::Verified(_)) {
                    progress_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            },
            move || stop_count.load(std::sync::atomic::Ordering::SeqCst) > 0,
        ))
        .unwrap();

    assert!(matches!(outcome, ApplyOutcome::Stopped { .. }));
    assert_eq!(adapter.writes, ["mithf.create_shift"]);
    let state = SyncState::open(&adapter.state_path).unwrap();
    assert_eq!(
        state
            .get_step(&request.shifts[0].key(), "mithf.create_shift")
            .unwrap()
            .unwrap()
            .status,
        "verified"
    );
    assert!(state
        .get_step(&request.shifts[0].key(), "mithf.assign_helper")
        .unwrap()
        .is_none());
    assert_eq!(snapshots(&adapter.state_path), 0);
}

#[test]
fn lost_create_response_survives_reopening_and_resumes_without_duplicate() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        timeout_after_create: true,
        ..Default::default()
    };
    let mut request = request();
    request.shifts[0].notes.clear();
    approve(&mut request, &adapter);
    assert!(matches!(
        runtime().block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {}
        )),
        Err(TransferError::Destination(_))
    ));
    let state = SyncState::open(&adapter.state_path).unwrap();
    assert_eq!(
        state
            .get_step(&request.shifts[0].key(), "mithf.create_shift")
            .unwrap()
            .unwrap()
            .status,
        "uncertain"
    );
    assert_eq!(snapshots(&adapter.state_path), 0);
    // Only a complete read without the shift retries the create.
    let saved_snapshot = adapter.snapshot.clone();
    adapter.snapshot = DestinationSnapshot::default();
    assert_eq!(
        preview_plan(&request, &adapter).items[0].outcome,
        Outcome::WouldCreate
    );
    adapter.snapshot = saved_snapshot;
    assert_eq!(
        preview_plan(&request, &adapter).items[0].outcome,
        Outcome::AlreadyMatched
    );
    approve(&mut request, &adapter);
    runtime()
        .block_on(apply_plan(
            request,
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ))
        .unwrap();
    assert_eq!(
        adapter
            .writes
            .iter()
            .filter(|key| *key == "mithf.create_shift")
            .count(),
        1
    );
}

#[test]
fn incomplete_reads_stale_approval_and_failed_readback_never_claim_success() {
    let temp = tempfile::tempdir().unwrap();
    for fail_read in [Some(1), Some(2), Some(3), None] {
        let path = temp.path().join(format!("state-{fail_read:?}.sqlite3"));
        let mut adapter = MemoryDestinations {
            state_path: path,
            fail_read,
            ignore_write: true,
            ..Default::default()
        };
        let mut request = request();
        approve(&mut request, &adapter);
        let error = runtime()
            .block_on(apply_plan(
                request.clone(),
                adapter.state_path.clone(),
                &mut adapter,
                |_| {},
            ))
            .unwrap_err();
        if fail_read.is_some() {
            assert!(matches!(error, TransferError::Destination(_)));
        } else {
            assert!(matches!(
                error,
                TransferError::Approval(ApprovalError::VerificationFailed)
            ));
        }
        assert_eq!(
            adapter.writes.len(),
            if matches!(fail_read, Some(1 | 2)) {
                0
            } else {
                1
            }
        );
        assert_eq!(snapshots(&adapter.state_path), 0);
        let state = SyncState::open(&adapter.state_path).unwrap();
        if !adapter.writes.is_empty() {
            assert_eq!(
                state
                    .get_step(&request.shifts[0].key(), "mithf.create_shift")
                    .unwrap()
                    .unwrap()
                    .status,
                "uncertain"
            );
        }
        assert!(state.exclusive_apply().is_ok());
    }
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("stale.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    request.shifts[0].ends_at += chrono::Duration::hours(1);
    assert!(matches!(
        runtime().block_on(apply_plan(
            request,
            adapter.state_path.clone(),
            &mut adapter,
            |_| {}
        )),
        Err(TransferError::Approval(ApprovalError::PlanChanged))
    ));
    assert!(adapter.writes.is_empty());
}

#[test]
fn cancellation_retains_uncertain_marker_and_releases_apply_lock() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        hang_on_write: true,
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    let started = adapter.write_started.clone();
    runtime().block_on(async {
        use std::{future::Future, task::Poll};
        let mut transfer = Box::pin(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ));
        tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| {
                assert!(transfer.as_mut().poll(cx).is_pending());
                if started.load(std::sync::atomic::Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        )
        .await
        .expect("write should start");
        drop(transfer);
    });
    assert_eq!(adapter.writes.len(), 1);
    let state = SyncState::open(&adapter.state_path).unwrap();
    assert_eq!(
        state
            .get_step(&request.shifts[0].key(), "mithf.create_shift")
            .unwrap()
            .unwrap()
            .status,
        "uncertain"
    );
    assert!(state.exclusive_apply().is_ok());
    assert_eq!(snapshots(&adapter.state_path), 0);
}

#[derive(Deserialize)]
struct Oracle {
    traces: Vec<Trace>,
}
#[derive(Deserialize)]
struct Trace {
    digest: String,
    config: PlanningConfig,
    shifts: Vec<SourceShift>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
    destination_before: DestinationSnapshot,
    destination_after: DestinationSnapshot,
    timeout_after_create: bool,
    records_before: Vec<Value>,
    records_after: Vec<Value>,
    writes: Vec<String>,
    error: Option<String>,
}

fn step_record(value: &Value) -> StepRecord {
    StepRecord {
        source_key: value["source_key"].as_str().unwrap().into(),
        step_key: value["step_key"].as_str().unwrap().into(),
        status: value["status"].as_str().unwrap().into(),
        source_hash: value["source_hash"].as_str().unwrap().into(),
        destination_id: value["destination_id"].as_str().map(str::to_owned),
        synced_payload: value["synced_payload"].as_object().unwrap().clone(),
        error: value["error"].as_str().map(str::to_owned),
    }
}

#[test]
fn complete_rust_runs_match_recorded_writes_and_recovery_records() {
    // Frozen at the Python removal cutover: 10 recorded apply traces, less the
    // one that blocked a retry of an unconfirmed create the shift plan now makes.
    let oracle: Oracle = serde_json::from_str(include_str!("goldens/approval.json")).unwrap();
    assert!(oracle.traces.len() >= 6);
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime();
    for (index, trace) in oracle.traces.into_iter().enumerate() {
        let path = temp.path().join(format!("run-{index}.sqlite3"));
        let state = SyncState::open(&path).unwrap();
        for record in &trace.records_before {
            state.record_step(&step_record(record), trace.now).unwrap();
        }
        let mut adapter = MemoryDestinations {
            state_path: path.clone(),
            snapshot: trace.destination_before,
            timeout_after_create: trace.timeout_after_create,
            ..Default::default()
        };
        let keys: Vec<_> = trace.shifts.iter().map(SourceShift::key).collect();
        let request = ApplyRequest {
            config: trace.config,
            shifts: trace.shifts,
            range_start: trace.start,
            range_end: trace.end,
            now: trace.now,
            expected_digest: trace.digest,
        };
        let result = runtime.block_on(apply_plan(request, path, &mut adapter, |_| {}));
        match trace.error {
            None => {
                result.unwrap();
            }
            Some(expected) => {
                let error = result.unwrap_err();
                let message = match error {
                    TransferError::Destination(source) => source.to_string(),
                    error => error.to_string(),
                };
                assert_eq!(message, expected, "run {index}");
            }
        }
        assert_eq!(adapter.writes, trace.writes, "run {index}");
        assert_eq!(adapter.snapshot, trace.destination_after, "run {index}");
        let actual: Vec<_> = keys
            .iter()
            .flat_map(|key| state.steps_for_source(key).unwrap())
            .collect();
        let expected: Vec<_> = trace.records_after.iter().map(step_record).collect();
        assert_eq!(actual, expected, "run {index}");
    }
}

#[test]
fn meeting_uses_full_shift_read_bounds_and_final_read_failure_leaves_no_source_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime();
    for fail_final in [false, true] {
        let mut adapter = MemoryDestinations {
            state_path: temp.path().join(format!("state-{fail_final}.sqlite3")),
            ..Default::default()
        };
        let mut request = request();
        request.shifts[0].title = "P-MØDE".into();
        request.shifts[0].notes.clear();
        request.range_start = request.shifts[0].starts_at + chrono::Duration::hours(1);
        request.range_end = request.shifts[0].ends_at - chrono::Duration::hours(1);
        // Initial read, 3 steps each with pre/post reads, final read.
        if fail_final {
            adapter.fail_read = Some(8);
        }
        approve(&mut request, &adapter);
        let result = runtime.block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {},
        ));
        if fail_final {
            assert!(matches!(result, Err(TransferError::Destination(_))));
        } else {
            result.unwrap();
        }
        assert!(adapter
            .ranges
            .iter()
            .all(|range| *range == (request.shifts[0].starts_at, request.shifts[0].ends_at)));
        assert_eq!(
            adapter.writes,
            [
                "mithf.create_shift",
                "mithf.assign_helper",
                "mithf.set_meeting"
            ]
        );
        assert_eq!(
            snapshots(&adapter.state_path),
            if fail_final { 0 } else { 1 }
        );
        let state = SyncState::open(&adapter.state_path).unwrap();
        assert_eq!(
            state.last_source_snapshot_at().unwrap().is_some(),
            !fail_final
        );
    }
}

#[test]
fn competing_apply_stops_before_reading_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let mut adapter = MemoryDestinations {
        state_path: temp.path().join("sync.sqlite3"),
        ..Default::default()
    };
    let mut request = request();
    approve(&mut request, &adapter);
    let state = SyncState::open(&adapter.state_path).unwrap();
    let guard = state.exclusive_apply().unwrap();
    let result = runtime().block_on(apply_plan(
        request,
        adapter.state_path.clone(),
        &mut adapter,
        |_| {},
    ));
    assert!(matches!(
        result,
        Err(TransferError::State(StateError::ApplyInProgress))
    ));
    assert_eq!(adapter.reads, 0);
    assert!(adapter.writes.is_empty());
    drop(guard);
}
