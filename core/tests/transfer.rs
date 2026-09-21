use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::{json, Value};
use teamup_shift_sync_core::{
    apply_plan, build_plan, plan_digest, ApplyRequest, ApprovalError, DestinationSnapshot,
    Destinations, DuosRegistration, MitHfShift, Outcome, PlanItem, PlanRequest, PlanningConfig,
    SourceShift, StateError, StepRecord, SyncState, TimeInterval, TransferError,
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
                shifts.retain(|s| s.id != id);
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
                    "id": format!("duos-{}", self.snapshot.duos_registrations.len()),
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

fn approve(request: &mut ApplyRequest, adapter: &MemoryDestinations) {
    let state = SyncState::open(&adapter.state_path).unwrap();
    let plan = build_plan(
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
    .unwrap();
    request.expected_digest = plan_digest(&plan).unwrap();
}

fn snapshots(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM source_occurrences", [], |r| r.get(0))
        .unwrap()
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
            |item| progress.push(item.step_key.clone()),
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
    let saved_snapshot = adapter.snapshot.clone();
    adapter.snapshot = DestinationSnapshot::default();
    approve(&mut request, &adapter);
    assert!(matches!(
        runtime().block_on(apply_plan(
            request.clone(),
            adapter.state_path.clone(),
            &mut adapter,
            |_| {}
        )),
        Err(TransferError::Approval(ApprovalError::UnresolvedItems))
    ));
    assert_eq!(adapter.writes.len(), 1);
    adapter.snapshot = saved_snapshot;
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
    // Frozen at the Python removal cutover: 10 recorded apply traces.
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
