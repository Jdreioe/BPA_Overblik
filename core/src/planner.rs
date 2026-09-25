//! Read-only planning. SQLite reads are blocking, so run this off the UI thread.
use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, FixedOffset};
use serde_json::{json, Map, Value};

use crate::{
    classify_source_title, parse_absences, parse_sps_instructions,
    reconciliation::{duos_payload, reconcile_duos, reconcile_mithf, shift_payload},
    segment_step,
    state::isoformat,
    AbsencePart, AbsenceReason, DestinationSnapshot, DuosAbsence, DuosRegistration, HelperMapping,
    MitHfShift, Outcome, ParseIssue, ParseIssueCode, PlanItem, PlanSystem, PlanningConfig,
    SourceShift, SourceTitle, SpsInterval, StateError, StepRecord, SyncPlan, SyncState,
    TimeInterval,
};

#[derive(Debug, thiserror::Error)]
pub enum PlanningError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("Stored synchronization segment key is invalid")]
    InvalidSegmentKey,
}

pub struct PlanRequest<'a> {
    pub config: &'a PlanningConfig,
    pub shifts: &'a [SourceShift],
    /// Must be a complete read for `reconciliation_range`, or planning is unsafe.
    pub destination: &'a DestinationSnapshot,
    pub range_start: DateTime<FixedOffset>,
    pub range_end: DateTime<FixedOffset>,
    pub now: DateTime<FixedOffset>,
    pub live: bool,
}

/// Include the full bounds of every selected shift in destination reads.
pub fn reconciliation_range(
    shifts: &[SourceShift],
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    shifts
        .iter()
        .filter(|s| s.ends_at > start && s.starts_at < end)
        .fold((start, end), |(from, to), s| {
            (from.min(s.starts_at), to.max(s.ends_at))
        })
}

/// A planned time change of an existing MitHF shift, keyed by its ID.
struct Move {
    source_key: String,
    step_key: String,
    to: TimeInterval,
}

type Moves = HashMap<String, Move>;

pub fn build_plan(request: &PlanRequest<'_>, state: &SyncState) -> Result<SyncPlan, PlanningError> {
    let mut shifts: Vec<_> = request
        .shifts
        .iter()
        .filter(|s| s.ends_at > request.range_start && s.starts_at < request.range_end)
        .collect();
    shifts.sort_by_key(|s| s.starts_at);
    // Plan once to learn which MitHF shifts change time, then again so every
    // other step is checked against where those will be. The first step to
    // claim a shift moves it. Shifts whose time changes go first, so a
    // shortened shift frees its hours before another shift is created in them.
    let mut items = plan_shifts(request, state, &shifts, &Moves::new())?;
    let mut moves = Moves::new();
    for item in &items {
        if item.outcome == Outcome::WouldUpdate && item.step_key.starts_with("mithf.create_shift") {
            if let Some(id) = &item.destination_id {
                moves.entry(id.clone()).or_insert_with(|| Move {
                    source_key: item.source_key.clone(),
                    step_key: item.step_key.clone(),
                    to: payload_interval(&item.payload),
                });
            }
        }
    }
    if !moves.is_empty() {
        let movers: BTreeSet<_> = moves.values().map(|m| m.source_key.clone()).collect();
        shifts.sort_by_key(|s| !movers.contains(&s.key()));
        items = plan_shifts(request, state, &shifts, &moves)?;
    }
    Ok(SyncPlan {
        starts_at: request.range_start,
        ends_at: request.range_end,
        generated_at: request.now,
        items,
    })
}

fn plan_shifts(
    request: &PlanRequest<'_>,
    state: &SyncState,
    shifts: &[&SourceShift],
    moves: &Moves,
) -> Result<Vec<PlanItem>, PlanningError> {
    let mut items = Vec::new();
    // A sick-reported MitHF shift is not worked. Only absent segments may
    // match one; everything else reconciles against the shifts people work.
    let healthy: Vec<MitHfShift> = request
        .destination
        .mithf_shifts
        .iter()
        .filter(|s| !s.sick)
        .cloned()
        .collect();
    for &shift in shifts {
        if classify_source_title(&shift.title) == SourceTitle::Reminder {
            items.push(item(
                shift,
                PlanSystem::Source,
                "source.reminder",
                Outcome::Excluded,
                "Shared 'Husk at checke' reminder, not a shift",
                "reminder",
            ));
            continue;
        }
        let parsed = parse_sps_instructions(shift, request.config.timezone);
        if !request.config.duos_enabled && !parsed.intervals.is_empty() {
            items.push(item(
                shift,
                PlanSystem::Source,
                "sps.no_duos",
                Outcome::Review,
                "SPS is written on this shift, but DUOS is disabled in setup",
                "sps_without_duos",
            ));
        }
        let absences = parse_absences(
            shift,
            &request.config.absences,
            &request.config.helpers,
            request.config.timezone,
        );
        for (prefix, issue) in parsed
            .issues
            .iter()
            .map(|i| ("sps", i))
            .chain(absences.issues.iter().map(|i| ("absence", i)))
        {
            items.push(issue_item(shift, prefix, issue));
        }
        let crossing = parsed
            .intervals
            .iter()
            .any(|sps| crosses_absence(&sps.interval, &absences.parts));
        if crossing {
            items.push(item(
                shift,
                PlanSystem::Source,
                "absence.sps_crossing",
                Outcome::Review,
                "An SPS interval starts before an absence and ends inside it, or the reverse",
                "sps_crosses_absence",
            ));
        }
        let Some(mapping) = request.config.helpers.get(&shift.helper_key) else {
            items.push(item(
                shift,
                PlanSystem::Mapping,
                "helper",
                Outcome::Review,
                &format!(
                    "No confirmed destination mapping for {} helper {}",
                    if shift.calendar_id.starts_with("sheet-") {
                        "spreadsheet"
                    } else if shift.calendar_id.starts_with("ical-") {
                        "calendar"
                    } else {
                        "TeamUp"
                    },
                    python_repr(&shift.helper_key)
                ),
                "no_helper_mapping",
            ));
            continue;
        };
        // Read once, keeping all errors visible rather than treating bad state as absent.
        let records = state.steps_for_source(&shift.key())?;
        let blocked = crossing
            || !absences.issues.is_empty()
            || parsed
                .issues
                .iter()
                .any(|i| i.code != ParseIssueCode::DuplicateUniInterval);
        let segments = split_segments(shift, &parsed.intervals, &absences.parts, blocked);
        let mut absent_segments = BTreeSet::new();
        for (index, segment) in segments.iter().enumerate() {
            if segment.absence.is_some() {
                absent_segments.insert(segment_step("mithf.report_sick", index));
            }
            plan_segment(
                request, shift, &records, index, segment, blocked, &healthy, moves, &mut items,
            );
        }
        for record in &records {
            let (base, suffix) = record
                .step_key
                .split_once('#')
                .unwrap_or((&record.step_key, ""));
            if base == "mithf.report_sick" && !absent_segments.contains(&record.step_key) {
                let mut removed = item(shift, PlanSystem::Mithf, &record.step_key, Outcome::Review, "MitHF has the helper reported absent, but the source no longer has that absence; no undo planned", "absence_removed");
                removed.payload = record.synced_payload.clone();
                removed.destination_id = record.destination_id.clone();
                items.push(removed);
            }
            if base != "mithf.create_shift" {
                continue;
            }
            let index = if suffix.is_empty() {
                0
            } else {
                suffix
                    .parse::<i64>()
                    .map_err(|_| PlanningError::InvalidSegmentKey)?
            };
            if index >= segments.len() as i64 {
                let mut retired = item(shift, PlanSystem::Mithf, &record.step_key, Outcome::Review, "A previously synchronized MitHF shift part is no longer in the source; no removal planned", "extra_segment_removed");
                retired.payload = record.synced_payload.clone();
                retired.destination_id = record.destination_id.clone();
                items.push(retired);
            }
        }
        let mut expected_steps = BTreeSet::new();
        for sps in &parsed.intervals {
            if !request.config.duos_enabled {
                continue;
            }
            // The SPS hours belong to whoever works them: the substitute
            // during an absence, the planned helper otherwise.
            let worker = segments
                .iter()
                .find(|s| s.absence.is_none() && s.intervals.iter().any(|i| i.key == sps.key))
                .map_or(mapping, |s| helper(request.config, s.helper));
            let key = format!("duos.interval:{}", sps.key);
            expected_steps.insert(key.clone());
            let expected = DuosRegistration {
                id: String::new(),
                arrangement_id: request.config.duos_arrangement_id.clone(),
                employee_number: worker.duos_employee_number.clone(),
                registration_type: request.config.duos_registration_type.clone(),
                starts_at: sps.interval.starts_at,
                ends_at: sps.interval.ends_at,
                status_id: 0,
            };
            items.push(plan_duos(
                request, shift, &records, &key, &expected, blocked,
            ));
            let Some(part) = absences.parts.iter().find(|p| {
                p.interval.starts_at <= sps.interval.starts_at
                    && sps.interval.ends_at <= p.interval.ends_at
            }) else {
                continue;
            };
            if blocked {
                continue;
            }
            // The absent helper's missed SPS hours, registered with the type
            // setup chose for this reason.
            let key = format!("duos.absence:{}", sps.key);
            let duos = request
                .config
                .absences
                .iter()
                .find(|m| m.reason == part.reason)
                .map_or(&DuosAbsence::Unset, |m| &m.duos);
            match duos {
                DuosAbsence::Skip => {}
                DuosAbsence::Unset => {
                    expected_steps.insert(key.clone());
                    let mut unset = item(
                        shift,
                        PlanSystem::Duos,
                        &key,
                        Outcome::Review,
                        "Choose how DUOS registers this absence in setup",
                        "absence_duos_type",
                    );
                    unset.payload = object(json!({"reason": part.reason}));
                    items.push(unset);
                }
                DuosAbsence::Type(registration_type) => {
                    expected_steps.insert(key.clone());
                    let expected = DuosRegistration {
                        employee_number: mapping.duos_employee_number.clone(),
                        registration_type: registration_type.clone(),
                        ..expected
                    };
                    items.push(plan_duos(
                        request, shift, &records, &key, &expected, blocked,
                    ));
                }
            }
        }
        for record in &records {
            if record.step_key.starts_with("duos.") && !expected_steps.contains(&record.step_key) {
                let mut removed = item(shift, PlanSystem::Duos, &record.step_key, Outcome::Review, "Previously synchronized SPS interval is no longer in the source; no destructive action planned", "removed_from_source");
                removed.payload = record.synced_payload.clone();
                removed.destination_id = record.destination_id.clone();
                items.push(removed);
            }
        }
    }
    Ok(items)
}

fn payload_interval(payload: &Map<String, Value>) -> TimeInterval {
    let time = |key: &str| {
        payload[key]
            .as_str()
            .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            .expect("planned shift payloads hold RFC 3339 times")
    };
    TimeInterval {
        starts_at: time("starts_at"),
        ends_at: time("ends_at"),
    }
}

struct Segment<'a> {
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
    intervals: Vec<&'a SpsInterval>,
    /// Source key of the helper who works the segment, or, when `absence` is
    /// set, of the planned helper MitHF reports absent for it.
    helper: &'a str,
    absence: Option<AbsenceReason>,
}

/// Without absences, one segment per SPS interval as before. An absence
/// becomes the planned helper's segment, reported absent, followed by the
/// substitute's segments; the hours around it stay the planned helper's.
fn split_segments<'a>(
    shift: &'a SourceShift,
    intervals: &'a [SpsInterval],
    parts: &'a [AbsencePart],
    blocked: bool,
) -> Vec<Segment<'a>> {
    // An unresolved instruction must never move a shift boundary.
    if blocked || parts.is_empty() {
        let all = intervals.iter().collect();
        return worked(
            shift.starts_at,
            shift.ends_at,
            all,
            &shift.helper_key,
            blocked,
        );
    }
    let mut runs = Vec::new();
    let mut cursor = shift.starts_at;
    for part in parts {
        if cursor < part.interval.starts_at {
            runs.push((cursor, part.interval.starts_at, None));
        }
        runs.push((part.interval.starts_at, part.interval.ends_at, Some(part)));
        cursor = part.interval.ends_at;
    }
    if cursor < shift.ends_at {
        runs.push((cursor, shift.ends_at, None));
    }
    let mut segments = Vec::new();
    for (starts_at, ends_at, part) in runs {
        let inside = intervals
            .iter()
            .filter(|sps| starts_at <= sps.interval.starts_at && sps.interval.starts_at < ends_at)
            .collect();
        let helper = match part {
            None => shift.helper_key.as_str(),
            Some(part) => {
                segments.push(Segment {
                    starts_at,
                    ends_at,
                    intervals: vec![],
                    helper: &shift.helper_key,
                    absence: Some(part.reason),
                });
                part.substitute.as_str()
            }
        };
        segments.extend(worked(starts_at, ends_at, inside, helper, false));
    }
    segments
}

/// One helper's time, cut at each further SPS start so every MitHF shift
/// carries at most one SPS interval.
fn worked<'a>(
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
    intervals: Vec<&'a SpsInterval>,
    helper: &'a str,
    blocked: bool,
) -> Vec<Segment<'a>> {
    if blocked || intervals.len() <= 1 {
        return vec![Segment {
            starts_at,
            ends_at,
            intervals,
            helper,
            absence: None,
        }];
    }
    intervals
        .iter()
        .enumerate()
        .map(|(index, sps)| Segment {
            starts_at: if index == 0 {
                starts_at
            } else {
                sps.interval.starts_at
            },
            ends_at: intervals
                .get(index + 1)
                .map(|next| next.interval.starts_at)
                .unwrap_or(ends_at),
            intervals: vec![*sps],
            helper,
            absence: None,
        })
        .collect()
}

/// An SPS interval that an absence boundary cuts through cannot be given to
/// either helper without guessing.
fn crosses_absence(sps: &TimeInterval, parts: &[AbsencePart]) -> bool {
    parts.iter().any(|part| {
        [part.interval.starts_at, part.interval.ends_at]
            .iter()
            .any(|edge| sps.starts_at < *edge && *edge < sps.ends_at)
    })
}

/// Absence lines only name mapped helpers, so a segment's helper always has one.
fn helper<'a>(config: &'a PlanningConfig, key: &str) -> &'a HelperMapping {
    config
        .helpers
        .get(key)
        .expect("segments only name mapped helpers")
}

fn issue_item(shift: &SourceShift, prefix: &str, issue: &ParseIssue) -> PlanItem {
    let code = serde_json::to_value(issue.code).expect("issue codes serialize as strings");
    let code = code.as_str().unwrap();
    item(
        shift,
        PlanSystem::Source,
        &format!("{prefix}:{}:{code}", issue.source_id),
        Outcome::Review,
        &issue.message,
        code,
    )
}

fn plan_duos(
    request: &PlanRequest<'_>,
    shift: &SourceShift,
    records: &[StepRecord],
    key: &str,
    expected: &DuosRegistration,
    blocked: bool,
) -> PlanItem {
    let mut planned = if blocked {
        item(shift, PlanSystem::Duos, key, Outcome::Review, "DUOS write is blocked because another SPS instruction on this shift could not be resolved safely", "source_issue")
    } else if expected.arrangement_id.is_empty() || expected.registration_type.is_empty() {
        item(
            shift,
            PlanSystem::Duos,
            key,
            Outcome::Review,
            "DUOS arrangement and registration type must be confirmed in configuration",
            "missing_configuration",
        )
    } else {
        let reconciled = reconcile_duos(
            expected,
            record(records, key),
            &request.destination.duos_registrations,
            &owned_by_others(records, "duos.", key),
        );
        let mut planned = item(
            shift,
            PlanSystem::Duos,
            key,
            reconciled.outcome,
            &reconciled.summary,
            reconciled.reason,
        );
        planned.destination_id = reconciled.matched.map(|m| m.id.clone());
        planned
    };
    planned.payload = duos_payload(expected);
    planned
}

#[allow(clippy::too_many_arguments)]
fn plan_segment(
    request: &PlanRequest<'_>,
    shift: &SourceShift,
    records: &[StepRecord],
    index: usize,
    segment: &Segment<'_>,
    blocked: bool,
    healthy: &[MitHfShift],
    moves: &Moves,
    items: &mut Vec<PlanItem>,
) {
    use Outcome::*;
    let mapping = helper(request.config, segment.helper);
    let shift_key = segment_step("mithf.create_shift", index);
    let candidates = if segment.absence.is_some() {
        &request.destination.mithf_shifts[..]
    } else {
        healthy
    };
    // MitHF says this helper was absent here, but the source says they
    // worked. Creating another shift beside the sick one would double it.
    if segment.absence.is_none()
        && request.destination.mithf_shifts.iter().any(|s| {
            s.sick
                && s.helper_name.as_ref() == Some(&mapping.mithf_name)
                && s.starts_at < segment.ends_at
                && s.ends_at > segment.starts_at
        })
    {
        let mut sick = item(
            shift,
            PlanSystem::Mithf,
            &shift_key,
            Review,
            "MitHF has the helper reported absent here, but the source has no absence line",
            "absent_in_mithf",
        );
        sick.payload = shift_payload(
            segment.starts_at,
            segment.ends_at,
            request.config.default_helper_count,
        );
        items.push(sick);
        return;
    }
    let moved: HashMap<_, _> = moves
        .iter()
        .filter(|(_, m)| m.source_key != shift.key() || m.step_key != shift_key)
        .map(|(id, m)| (id.clone(), m.to.clone()))
        .collect();
    let assign_key = segment_step("mithf.assign_helper", index);
    let sps_key = segment_step("mithf.set_sps", index);
    let reconciled = reconcile_mithf(
        segment.starts_at,
        segment.ends_at,
        request.config.default_helper_count,
        &mapping.mithf_name,
        record(records, &shift_key),
        candidates,
        &owned_by_others(records, "mithf.create_shift", &shift_key),
        &moved,
    );
    let matched = reconciled.matched;
    let destination_id = matched.map(|m| m.id.clone());
    let mut create = item(
        shift,
        PlanSystem::Mithf,
        &shift_key,
        reconciled.outcome,
        &reconciled.summary,
        reconciled.reason,
    );
    create.payload = shift_payload(
        segment.starts_at,
        segment.ends_at,
        request.config.default_helper_count,
    );
    create.destination_id = destination_id.clone();
    items.push(create);

    let (outcome, summary, reason) = match matched {
        None if reconciled.outcome == Conflicted => (
            Conflicted,
            "Helper assignment is blocked by the shift conflict".into(),
            "blocked_by_shift",
        ),
        None => (
            WouldCreate,
            format!(
                "Would assign {} after creating the shift",
                mapping.mithf_name
            ),
            "",
        ),
        Some(m) if m.helper_name.as_ref() == Some(&mapping.mithf_name) => (
            AlreadyMatched,
            format!("MitHF helper is already {}", mapping.mithf_name),
            "",
        ),
        Some(m) if m.helper_name.is_none() => (
            WouldUpdate,
            format!(
                "Would resume on the existing shift and assign {}",
                mapping.mithf_name
            ),
            "",
        ),
        Some(m) => (
            Conflicted,
            format!(
                "Existing MitHF shift is assigned to {}, not {}",
                m.helper_name.as_deref().unwrap(),
                mapping.mithf_name
            ),
            "assigned_to_other",
        ),
    };
    let mut assignment = item(
        shift,
        PlanSystem::Mithf,
        &assign_key,
        outcome,
        &summary,
        reason,
    );
    assignment.payload = object(json!({"helper_name": mapping.mithf_name}));
    assignment.destination_id = destination_id.clone();
    let assignment_outcome = assignment.outcome;
    items.push(assignment);

    if let Some(reason) = segment.absence {
        let key = segment_step("mithf.report_sick", index);
        let expected = object(json!({"helper_name": mapping.mithf_name, "reason": reason}));
        let (outcome, summary, why) =
            if reconciled.outcome == Conflicted || assignment_outcome == Conflicted {
                (
                    Conflicted,
                    "The absence is blocked by the shift or helper conflict",
                    "blocked_by_shift",
                )
            } else if matched.is_some_and(|m| m.sick) {
                // MitHF does not show the reason, so only a recorded one can
                // tell that the source changed it afterwards.
                if record(records, &key).is_some_and(|r| r.synced_payload != expected) {
                    (
                        Review,
                        "The absence reason changed after MitHF was told; change it in MitHF",
                        "absence_reason_changed",
                    )
                } else {
                    (
                        AlreadyMatched,
                        "MitHF already has the helper reported absent",
                        "",
                    )
                }
            } else if request.live {
                (WouldCreate, "Would report the helper absent in MitHF", "")
            } else {
                (
                    PendingIntegration,
                    "The absence requires applying; live destination integration was not selected",
                    "offline_preview",
                )
            };
        let mut absent = item(shift, PlanSystem::Mithf, &key, outcome, summary, why);
        absent.payload = expected;
        absent.destination_id = destination_id.clone();
        items.push(absent);
    }

    let sps_record = record(records, &sps_key);
    if !segment.intervals.is_empty() {
        let mut expected: Vec<_> = segment
            .intervals
            .iter()
            .map(|s| s.interval.clone())
            .collect();
        let mut actual = matched.map(|m| m.sps_intervals.clone()).unwrap_or_default();
        let expected_payload = intervals_payload(&expected);
        expected.sort_by_key(|i| i.starts_at);
        actual.sort_by_key(|i| i.starts_at);
        let (outcome, summary, reason) = if blocked {
            (
                Review,
                "MitHF SPS is blocked by unresolved source instructions",
                "source_issue",
            )
        } else if reconciled.outcome == Conflicted || assignment_outcome == Conflicted {
            (
                Conflicted,
                "MitHF SPS is blocked by the shift or helper conflict",
                "blocked_by_shift",
            )
        } else if matched.is_some() && actual == expected {
            (
                AlreadyMatched,
                "Existing MitHF SPS intervals match exactly",
                "",
            )
        } else if actual.len() > 1 {
            // MitHF edits one SPS record at a time; extra ones need deleting.
            (
                Review,
                "The MitHF shift has more than one SPS interval; delete the extra ones",
                "extra_intervals",
            )
        } else if request.live {
            (
                if actual.is_empty() {
                    WouldCreate
                } else {
                    WouldUpdate
                },
                "Would set the MitHF SPS interval",
                "",
            )
        } else {
            (
                PendingIntegration,
                "SPS interval requires applying; live destination integration was not selected",
                "offline_preview",
            )
        };
        let mut sps = item(shift, PlanSystem::Mithf, &sps_key, outcome, summary, reason);
        sps.payload = expected_payload;
        sps.destination_id = destination_id.clone();
        items.push(sps);
    } else if segment.absence.is_some() {
        // SPS moves to the substitute's shift. MitHF would count it twice if
        // it stayed on the absent helper's, and the app never deletes.
        if matched.is_some_and(|m| !m.sps_intervals.is_empty()) {
            let mut stale = item(
                shift,
                PlanSystem::Mithf,
                &sps_key,
                Review,
                "SPS is still on the absent helper's MitHF shift; remove it there",
                "sps_on_absent_shift",
            );
            stale.destination_id = destination_id.clone();
            items.push(stale);
        }
    } else if let Some(record) = sps_record {
        let mut removed = item(
            shift,
            PlanSystem::Mithf,
            &sps_key,
            Review,
            "Previously synchronized SPS is no longer in the source; no removal planned",
            "removed_from_source",
        );
        removed.payload = record.synced_payload.clone();
        removed.destination_id = record.destination_id.clone();
        items.push(removed);
    }

    if let (SourceTitle::Meeting(category), None) =
        (classify_source_title(&shift.title), segment.absence)
    {
        let key = segment_step("mithf.set_meeting", index);
        let expected = [TimeInterval {
            starts_at: segment.starts_at,
            ends_at: segment.ends_at,
        }];
        let actual = matched
            .map(|m| m.meeting_intervals.as_slice())
            .unwrap_or_default();
        let (outcome, reason) =
            if reconciled.outcome == Conflicted || assignment_outcome == Conflicted {
                (Conflicted, "blocked_by_shift")
            } else if actual == expected {
                (AlreadyMatched, "")
            } else if actual.len() > 1 {
                (Review, "extra_intervals")
            } else if request.live {
                (
                    if actual.is_empty() {
                        WouldCreate
                    } else {
                        WouldUpdate
                    },
                    "",
                )
            } else {
                (PendingIntegration, "offline_preview")
            };
        let mut meeting = item(
            shift,
            PlanSystem::Mithf,
            &key,
            outcome,
            "Vagtmøde for the full shift",
            reason,
        );
        meeting.payload = object(
            json!({"category": category.name, "category_code": category.code, "starts_at": isoformat(segment.starts_at), "ends_at": isoformat(segment.ends_at)}),
        );
        meeting.destination_id = destination_id;
        items.push(meeting);
    }
}

/// Destination IDs recorded by this shift's other steps of the same kind.
fn owned_by_others(records: &[StepRecord], kind: &str, key: &str) -> BTreeSet<String> {
    records
        .iter()
        .filter(|r| r.step_key.starts_with(kind) && r.step_key != key)
        .filter_map(|r| r.destination_id.clone())
        .collect()
}

fn record<'a>(records: &'a [StepRecord], key: &str) -> Option<&'a StepRecord> {
    records.iter().find(|r| r.step_key == key)
}

fn item(
    shift: &SourceShift,
    system: PlanSystem,
    key: &str,
    outcome: Outcome,
    summary: &str,
    reason: &str,
) -> PlanItem {
    PlanItem {
        source_key: shift.key(),
        system,
        step_key: key.into(),
        outcome,
        summary: summary.into(),
        payload: Map::new(),
        destination_id: None,
        reason: reason.into(),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("planner payloads are objects")
        .clone()
}

fn intervals_payload(intervals: &[TimeInterval]) -> Map<String, Value> {
    object(
        json!({"intervals": intervals.iter().map(|i| json!({"starts_at": isoformat(i.starts_at), "ends_at": isoformat(i.ends_at)})).collect::<Vec<_>>()}),
    )
}

// Match the legacy user-facing repr for helper keys, including quoted names.
fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if c.is_control() && (c as u32) < 256 => {
                result.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}
