//! Destination identity and conflict rules shared by each planned segment.
use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, FixedOffset};
use serde_json::{json, Map, Value};

use crate::{state::isoformat, DuosRegistration, MitHfShift, Outcome, StepRecord, TimeInterval};

pub(crate) struct Reconciled<'a, T> {
    pub matched: Option<&'a T>,
    pub outcome: Outcome,
    pub summary: String,
    pub reason: &'static str,
}

fn result<'a, T>(
    matched: Option<&'a T>,
    outcome: Outcome,
    summary: impl Into<String>,
    reason: &'static str,
) -> Reconciled<'a, T> {
    Reconciled {
        matched,
        outcome,
        summary: summary.into(),
        reason,
    }
}

pub(crate) fn shift_payload(
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
    helper_count: i64,
) -> Map<String, Value> {
    json!({"starts_at": isoformat(starts_at), "ends_at": isoformat(ends_at), "helper_count": helper_count}).as_object().unwrap().clone()
}

pub(crate) fn duos_payload(item: &DuosRegistration) -> Map<String, Value> {
    json!({
        "arrangement_id": item.arrangement_id,
        "employee_number": item.employee_number,
        "registration_type": item.registration_type,
        "starts_at": isoformat(item.starts_at),
        "ends_at": isoformat(item.ends_at),
    })
    .as_object()
    .unwrap()
    .clone()
}

/// `owned` holds destinations recorded by the same source shift's other steps.
/// A step without its own record never mistakes them for a duplicate or an
/// overlap: their own steps move, match or block them first, as when a new SPS
/// interval splits a transferred shift into more parts.
///
/// `moved` holds shifts that other steps change to the given times; those steps
/// run first. They are checked where they will be, so a helper taking over the
/// end of another's shift is created after that shift is shortened.
///
/// Without a record, the one overlapping shift is adopted and its time changed
/// when it is already `helper_name`'s and shares the source's start or end:
/// the same shift entered by hand, then shortened or extended in the source.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconcile_mithf<'a>(
    starts_at: DateTime<FixedOffset>,
    ends_at: DateTime<FixedOffset>,
    helper_count: i64,
    helper_name: &str,
    record: Option<&StepRecord>,
    candidates: &'a [MitHfShift],
    owned: &BTreeSet<String>,
    moved: &HashMap<String, TimeInterval>,
) -> Reconciled<'a, MitHfShift> {
    use Outcome::*;
    let expected = shift_payload(starts_at, ends_at, helper_count);
    if let Some(record) =
        record.filter(|r| r.destination_id.as_ref().is_some_and(|id| !id.is_empty()))
    {
        let Some(known) = candidates
            .iter()
            .find(|c| Some(&c.id) == record.destination_id.as_ref())
        else {
            return result(None, Conflicted, "Previously synchronized MitHF shift is missing; it will not be recreated automatically", "destination_missing");
        };
        let actual = shift_payload(known.starts_at, known.ends_at, known.helper_count);
        if actual == expected {
            return result(
                Some(known),
                AlreadyMatched,
                "MitHF shift already matches",
                "",
            );
        }
        if actual == record.synced_payload {
            return result(
                Some(known),
                WouldUpdate,
                "Source shift changed; destination still matches the last synchronized values",
                "",
            );
        }
        return result(
            Some(known),
            Conflicted,
            "MitHF shift was manually changed; automatic update is blocked",
            "manually_changed",
        );
    }
    let foreign = || candidates.iter().filter(|c| !owned.contains(&c.id));
    let exact: Vec<_> = foreign()
        .filter(|c| !moved.contains_key(&c.id))
        .filter(|c| shift_payload(c.starts_at, c.ends_at, c.helper_count) == expected)
        .collect();
    if exact.len() == 1 {
        return result(
            Some(exact[0]),
            AlreadyMatched,
            "Existing MitHF shift matches by date, time, and helper count",
            "",
        );
    }
    if exact.len() > 1 {
        return result(
            None,
            Conflicted,
            format!(
                "Multiple MitHF shifts match; destination identity is not unique: {}",
                describe_shifts(exact)
            ),
            "not_unique",
        );
    }
    if record.is_some_and(|r| r.status == "uncertain") {
        return result(
            None,
            Conflicted,
            "Previous MitHF request has an uncertain outcome; manual reconciliation required",
            "uncertain_write",
        );
    }
    let overlapping: Vec<_> = foreign()
        .filter(|c| {
            let (from, to) = moved
                .get(&c.id)
                .map_or((c.starts_at, c.ends_at), |i| (i.starts_at, i.ends_at));
            from < ends_at && to > starts_at
        })
        .collect();
    if let [shift] = overlapping[..] {
        if !moved.contains_key(&shift.id)
            && shift.helper_name.as_deref() == Some(helper_name)
            && shift.helper_count == helper_count
            && (shift.starts_at == starts_at || shift.ends_at == ends_at)
        {
            return result(
                Some(shift),
                WouldUpdate,
                "The helper's MitHF shift was entered by hand; its time would change to match the source",
                "",
            );
        }
    }
    if !overlapping.is_empty() {
        return result(None, Conflicted, format!("Existing MitHF shifts overlap the source interval {} to {}; review split or changed shifts before creating another: {}", isoformat(starts_at), isoformat(ends_at), describe_shifts(overlapping)), "overlapping");
    }
    result(None, WouldCreate, "Would create the MitHF shift", "")
}

fn describe_shifts(mut shifts: Vec<&MitHfShift>) -> String {
    shifts.sort_by_key(|s| s.starts_at);
    shifts
        .into_iter()
        .map(|s| {
            let helper = s
                .helper_name
                .as_ref()
                .filter(|name| !name.is_empty())
                .map(|name| format!(" ({name})"))
                .unwrap_or_default();
            format!(
                "{} {} to {}{}",
                s.id,
                isoformat(s.starts_at),
                isoformat(s.ends_at),
                helper
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `owned` works as for `reconcile_mithf`, for the shift's other SPS intervals.
pub(crate) fn reconcile_duos<'a>(
    expected: &DuosRegistration,
    record: Option<&StepRecord>,
    candidates: &'a [DuosRegistration],
    owned: &BTreeSet<String>,
) -> Reconciled<'a, DuosRegistration> {
    use Outcome::*;
    let payload = duos_payload(expected);
    if let Some(record) =
        record.filter(|r| r.destination_id.as_ref().is_some_and(|id| !id.is_empty()))
    {
        let Some(known) = candidates
            .iter()
            .find(|c| Some(&c.id) == record.destination_id.as_ref())
        else {
            return result(None, Conflicted, "Previously synchronized DUOS registration is missing; no replacement will be added", "destination_missing");
        };
        let actual = duos_payload(known);
        if actual == payload {
            if matches!(known.status_id, 2 | 5 | 6) {
                return result(
                    Some(known),
                    Review,
                    "DUOS registration is rejected, withdrawn, or offsetting",
                    "rejected",
                );
            }
            return result(
                Some(known),
                AlreadyMatched,
                "DUOS registration already matches",
                "",
            );
        }
        if actual == record.synced_payload {
            if known.status_id != 0 {
                return result(
                    Some(known),
                    Review,
                    "DUOS registration is no longer pending; review changes manually",
                    "not_pending",
                );
            }
            return result(
                Some(known),
                WouldUpdate,
                "SPS source changed; DUOS still matches the last synchronized values",
                "",
            );
        }
        return result(
            Some(known),
            Conflicted,
            "DUOS registration was manually changed; automatic update is blocked",
            "manually_changed",
        );
    }
    let foreign = || candidates.iter().filter(|c| !owned.contains(&c.id));
    let exact: Vec<_> = foreign().filter(|c| duos_payload(c) == payload).collect();
    if exact.len() == 1 {
        if matches!(exact[0].status_id, 2 | 5 | 6) {
            return result(
                Some(exact[0]),
                Review,
                "Matching DUOS registration is rejected, withdrawn, or offsetting",
                "rejected",
            );
        }
        return result(
            Some(exact[0]),
            AlreadyMatched,
            "Existing DUOS registration matches exactly",
            "",
        );
    }
    if exact.len() > 1 {
        return result(
            None,
            Conflicted,
            "Multiple DUOS registrations match; destination identity is not unique",
            "not_unique",
        );
    }
    if record.is_some_and(|r| r.status == "uncertain") {
        return result(
            None,
            Conflicted,
            "Previous DUOS request has an uncertain outcome; manual reconciliation required",
            "uncertain_write",
        );
    }
    if foreign().any(|c| {
        c.employee_number == expected.employee_number
            && c.arrangement_id == expected.arrangement_id
            && c.starts_at < expected.ends_at
            && c.ends_at > expected.starts_at
    }) {
        return result(None, Conflicted, "Existing DUOS hours overlap this interval; review before creating another registration", "overlapping");
    }
    result(None, WouldCreate, "Would create the DUOS registration", "")
}
