from __future__ import annotations

from collections.abc import Iterable
from datetime import datetime
from typing import Any
from zoneinfo import ZoneInfo

from .models import (
    AppConfig,
    DestinationSnapshot,
    DuosRegistration,
    HelperMapping,
    MitHfShift,
    Outcome,
    PlanItem,
    SourceShift,
    SpsInterval,
    SyncPlan,
    TimeInterval,
    segment_step,
    step_base,
    step_segment,
)
from .parser import parse_sps_instructions
from .source_rules import is_reminder, meeting_item
from .state import StepRecord, SyncState

#: One MitHF shift segment: its bounds and the SPS intervals planned on it.
Segment = tuple[datetime, datetime, tuple[SpsInterval, ...]]


def build_plan(
    *,
    config: AppConfig,
    shifts: Iterable[SourceShift],
    destination: DestinationSnapshot,
    range_start: datetime,
    range_end: datetime,
    now: datetime,
    state: SyncState,
    live: bool = False,
) -> SyncPlan:
    timezone = ZoneInfo(config.timezone)
    items: list[PlanItem] = []
    for shift in sorted(shifts, key=lambda value: value.starts_at):
        if shift.ends_at <= range_start or shift.starts_at >= range_end:
            continue
        if is_reminder(shift.title):
            items.append(
                PlanItem(
                    shift.key,
                    "source",
                    "source.reminder",
                    Outcome.EXCLUDED,
                    "Shared 'Husk at checke' reminder, not a shift",
                    reason="reminder",
                )
            )
            continue
        items.extend(
            _plan_shift(config, shift, destination, now, timezone, state, live)
        )
    return SyncPlan(range_start, range_end, now, tuple(items))


def split_segments(
    shift: SourceShift, intervals: tuple[SpsInterval, ...]
) -> tuple[Segment, ...]:
    """Split a source shift into the MitHF shifts needed to carry its SPS time.

    MitHF stores at most one SPS interval per shift, so a shift with several
    intervals becomes several consecutive MitHF shifts. Each cut is at the
    start of the next SPS interval: ``uni 8-10 & 13-14`` on a 07:30–24:00
    shift becomes 07:30–13:00 carrying 08:00–10:00 and 13:00–24:00 carrying
    13:00–14:00. The parser guarantees the intervals are sorted, do not
    overlap and lie inside the shift, so every segment is non-empty and
    contains exactly its own interval.
    """
    if len(intervals) <= 1:
        return ((shift.starts_at, shift.ends_at, intervals),)
    bounds = [
        shift.starts_at,
        *(item.interval.starts_at for item in intervals[1:]),
        shift.ends_at,
    ]
    return tuple(
        (bounds[index], bounds[index + 1], (intervals[index],))
        for index in range(len(intervals))
    )


def _plan_shift(
    config: AppConfig,
    shift: SourceShift,
    destination: DestinationSnapshot,
    now: datetime,
    timezone: ZoneInfo,
    state: SyncState,
    live: bool,
) -> list[PlanItem]:
    items: list[PlanItem] = []
    mapping = config.helpers.get(shift.helper_key)
    parsed = parse_sps_instructions(shift, timezone)

    for issue in parsed.issues:
        items.append(
            PlanItem(
                shift.key,
                "source",
                f"sps:{issue.source_id}:{issue.code}",
                Outcome.REVIEW,
                issue.message,
                reason=issue.code,
            )
        )

    if mapping is None:
        items.append(
            PlanItem(
                shift.key,
                "mapping",
                "helper",
                Outcome.REVIEW,
                f"No confirmed destination mapping for TeamUp helper {shift.helper_key!r}",
                reason="no_helper_mapping",
            )
        )
        return items

    # An unresolved instruction must never move a shift boundary: keep the
    # shift whole and report the SPS step for review instead of guessing.
    source_blocked = any(
        issue.code != "duplicate_uni_interval" for issue in parsed.issues
    )
    segments = split_segments(shift, () if source_blocked else parsed.intervals)
    if source_blocked:
        start, end, _ = segments[0]
        segments = ((start, end, parsed.intervals),)

    for index, (starts_at, ends_at, intervals) in enumerate(segments):
        items.extend(
            _plan_segment(
                config=config,
                shift=shift,
                mapping=mapping,
                destination=destination,
                state=state,
                live=live,
                index=index,
                starts_at=starts_at,
                ends_at=ends_at,
                intervals=intervals,
                source_blocked=source_blocked,
            )
        )
    items.extend(_retired_segment_items(state, shift, len(segments)))
    items.extend(
        _plan_duos(
            config, shift, mapping, parsed, destination, state, now, source_blocked
        )
    )
    return items


def _plan_segment(
    *,
    config: AppConfig,
    shift: SourceShift,
    mapping: HelperMapping,
    destination: DestinationSnapshot,
    state: SyncState,
    live: bool,
    index: int,
    starts_at: datetime,
    ends_at: datetime,
    intervals: tuple[SpsInterval, ...],
    source_blocked: bool,
) -> list[PlanItem]:
    items: list[PlanItem] = []
    shift_step = segment_step("mithf.create_shift", index)
    assign_step = segment_step("mithf.assign_helper", index)
    sps_step = segment_step("mithf.set_sps", index)

    shift_payload = {
        "starts_at": starts_at.isoformat(),
        "ends_at": ends_at.isoformat(),
        "helper_count": config.default_helper_count,
    }
    create_step = state.get_step(shift.key, shift_step)
    mithf_match, shift_outcome, shift_summary, shift_reason = _reconcile_mithf_shift(
        shift_payload, create_step, destination.mithf_shifts
    )
    items.append(
        PlanItem(
            shift.key,
            "mithf",
            shift_step,
            shift_outcome,
            shift_summary,
            shift_payload,
            mithf_match.id if mithf_match else None,
            reason=shift_reason,
        )
    )

    assignment_payload = {"helper_name": mapping.mithf_name}
    assignment_reason = ""
    if mithf_match is None:
        assignment_outcome = (
            Outcome.CONFLICTED
            if shift_outcome == Outcome.CONFLICTED
            else Outcome.WOULD_CREATE
        )
        if assignment_outcome == Outcome.CONFLICTED:
            assignment_summary = "Helper assignment is blocked by the shift conflict"
            assignment_reason = "blocked_by_shift"
        else:
            assignment_summary = (
                f"Would assign {mapping.mithf_name} after creating the shift"
            )
    elif mithf_match.helper_name == mapping.mithf_name:
        assignment_outcome = Outcome.ALREADY_MATCHED
        assignment_summary = f"MitHF helper is already {mapping.mithf_name}"
    elif mithf_match.helper_name is None:
        assignment_outcome = Outcome.WOULD_UPDATE
        assignment_summary = (
            f"Would resume on the existing shift and assign {mapping.mithf_name}"
        )
    else:
        assignment_outcome = Outcome.CONFLICTED
        assignment_summary = f"Existing MitHF shift is assigned to {mithf_match.helper_name}, not {mapping.mithf_name}"
        assignment_reason = "assigned_to_other"
    assignment_record = state.get_step(shift.key, assign_step)
    if (
        assignment_record
        and assignment_record.status == "uncertain"
        and assignment_outcome != Outcome.ALREADY_MATCHED
    ):
        assignment_outcome = Outcome.CONFLICTED
        assignment_summary = "Previous helper assignment has an uncertain outcome; review before retrying"
        assignment_reason = "uncertain_write"
    items.append(
        PlanItem(
            shift.key,
            "mithf",
            assign_step,
            assignment_outcome,
            assignment_summary,
            assignment_payload,
            mithf_match.id if mithf_match else None,
            reason=assignment_reason,
        )
    )

    sps_record = state.get_step(shift.key, sps_step)
    if intervals:
        expected_intervals = tuple(item.interval for item in intervals)
        sps_payload = {
            "intervals": [_interval_payload(item) for item in expected_intervals]
        }
        actual_intervals = mithf_match.sps_intervals if mithf_match else ()
        actual_payload = {
            "intervals": [_interval_payload(item) for item in actual_intervals]
        }
        sps_reason = ""
        if source_blocked:
            sps_outcome = Outcome.REVIEW
            sps_summary = "MitHF SPS is blocked by unresolved source instructions"
            sps_reason = "source_issue"
        elif (
            shift_outcome == Outcome.CONFLICTED
            or assignment_outcome == Outcome.CONFLICTED
        ):
            sps_outcome = Outcome.CONFLICTED
            sps_summary = "MitHF SPS is blocked by the shift or helper conflict"
            sps_reason = "blocked_by_shift"
        elif mithf_match and sorted(
            actual_intervals, key=lambda item: item.starts_at
        ) == sorted(expected_intervals, key=lambda item: item.starts_at):
            sps_outcome = Outcome.ALREADY_MATCHED
            sps_summary = "Existing MitHF SPS intervals match exactly"
        elif sps_record and actual_payload != sps_record.synced_payload:
            sps_outcome = Outcome.CONFLICTED
            sps_summary = "MitHF SPS changed since synchronization; automatic replacement is blocked"
            sps_reason = "changed_since_sync"
        elif actual_intervals and sps_record is None:
            sps_outcome = Outcome.REVIEW
            sps_summary = (
                "Existing MitHF SPS differs from the source; review before replacing it"
            )
            sps_reason = "existing_differs"
        else:
            sps_outcome = (
                (Outcome.WOULD_UPDATE if actual_intervals else Outcome.WOULD_CREATE)
                if live
                else Outcome.PENDING_INTEGRATION
            )
            sps_summary = (
                "Would set the MitHF SPS interval"
                if live
                else "SPS interval requires applying; live destination integration was not selected"
            )
            if not live:
                sps_reason = "offline_preview"
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                sps_step,
                sps_outcome,
                sps_summary,
                sps_payload,
                mithf_match.id if mithf_match else None,
                reason=sps_reason,
            )
        )
    elif sps_record:
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                sps_step,
                Outcome.REVIEW,
                "Previously synchronized SPS is no longer in the source; no removal planned",
                sps_record.synced_payload,
                sps_record.destination_id,
                reason="removed_from_source",
            )
        )

    meeting = meeting_item(shift.title, shift.key, starts_at, ends_at)
    if meeting:
        meeting_step = segment_step(meeting.step_key, index)
        expected = (TimeInterval(starts_at, ends_at),)
        existing = mithf_match.meeting_intervals if mithf_match else ()
        reason = ""
        if (
            shift_outcome == Outcome.CONFLICTED
            or assignment_outcome == Outcome.CONFLICTED
        ):
            outcome = Outcome.CONFLICTED
            reason = "blocked_by_shift"
        elif existing == expected:
            outcome = Outcome.ALREADY_MATCHED
        elif existing:
            outcome = Outcome.REVIEW
            reason = "existing_differs"
        else:
            outcome = Outcome.WOULD_CREATE if live else Outcome.PENDING_INTEGRATION
            if not live:
                reason = "offline_preview"
        meeting_record = state.get_step(shift.key, meeting_step)
        if (
            meeting_record
            and meeting_record.status == "uncertain"
            and outcome != Outcome.ALREADY_MATCHED
        ):
            outcome = Outcome.CONFLICTED
            reason = "uncertain_write"
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                meeting_step,
                outcome,
                "Vagtmøde for the full shift; existing differences require review",
                meeting.payload,
                mithf_match.id if mithf_match else None,
                reason=reason,
            )
        )
    return items


def _retired_segment_items(
    state: SyncState, shift: SourceShift, active: int
) -> list[PlanItem]:
    """Report MitHF shifts created for SPS intervals the source no longer has.

    Removing an SPS interval shrinks the number of segments. The extra MitHF
    shift stays in the destination; deleting it is never automatic.
    """
    items: list[PlanItem] = []
    for record in state.steps_for_source(shift.key):
        if (
            step_base(record.step_key) != "mithf.create_shift"
            or step_segment(record.step_key) < active
        ):
            continue
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                record.step_key,
                Outcome.REVIEW,
                "A previously synchronized MitHF shift part is no longer in the source; no removal planned",
                record.synced_payload,
                record.destination_id,
                reason="extra_segment_removed",
            )
        )
    return items


def _plan_duos(
    config: AppConfig,
    shift: SourceShift,
    mapping: HelperMapping,
    parsed: Any,
    destination: DestinationSnapshot,
    state: SyncState,
    now: datetime,
    source_blocked: bool,
) -> list[PlanItem]:
    items: list[PlanItem] = []
    expected_steps: set[str] = set()
    for sps in parsed.intervals:
        step_key = f"duos.interval:{sps.key}"
        expected_steps.add(step_key)
        payload = {
            "arrangement_id": config.duos_arrangement_id,
            "employee_number": mapping.duos_employee_number,
            "registration_type": config.duos_registration_type,
            **_interval_payload(sps.interval),
        }
        if source_blocked:
            items.append(
                PlanItem(
                    shift.key,
                    "duos",
                    step_key,
                    Outcome.REVIEW,
                    "DUOS write is blocked because another SPS instruction on this shift could not be resolved safely",
                    payload,
                    reason="source_issue",
                )
            )
            continue
        if not config.duos_arrangement_id or not config.duos_registration_type:
            items.append(
                PlanItem(
                    shift.key,
                    "duos",
                    step_key,
                    Outcome.REVIEW,
                    "DUOS arrangement and registration type must be confirmed in configuration",
                    payload,
                    reason="missing_configuration",
                )
            )
            continue
        if sps.interval.ends_at > now:
            items.append(
                PlanItem(
                    shift.key,
                    "duos",
                    step_key,
                    Outcome.EXCLUDED,
                    "SPS interval is ongoing or in the future; DUOS is retrospective only",
                    payload,
                    reason="future_hours",
                )
            )
            continue
        record = state.get_step(shift.key, step_key)
        match, outcome, summary, reason = _reconcile_duos(
            payload, record, destination.duos_registrations
        )
        items.append(
            PlanItem(
                shift.key,
                "duos",
                step_key,
                outcome,
                summary,
                payload,
                match.id if match else None,
                reason=reason,
            )
        )

    for record in state.steps_for_source(shift.key):
        if (
            record.step_key.startswith("duos.interval:")
            and record.step_key not in expected_steps
        ):
            items.append(
                PlanItem(
                    shift.key,
                    "duos",
                    record.step_key,
                    Outcome.REVIEW,
                    "Previously synchronized SPS interval is no longer in the source; no destructive action planned",
                    record.synced_payload,
                    record.destination_id,
                    reason="removed_from_source",
                )
            )
    return items


def _reconcile_mithf_shift(
    expected: dict[str, Any],
    record: StepRecord | None,
    candidates: tuple[MitHfShift, ...],
) -> tuple[MitHfShift | None, Outcome, str, str]:
    if record and record.destination_id:
        known = next(
            (item for item in candidates if item.id == record.destination_id), None
        )
        if known is None:
            return (
                None,
                Outcome.CONFLICTED,
                "Previously synchronized MitHF shift is missing; it will not be recreated automatically",
                "destination_missing",
            )
        actual = {
            "starts_at": known.starts_at.isoformat(),
            "ends_at": known.ends_at.isoformat(),
            "helper_count": known.helper_count,
        }
        if actual == expected:
            return known, Outcome.ALREADY_MATCHED, "MitHF shift already matches", ""
        if actual == record.synced_payload:
            return (
                known,
                Outcome.WOULD_UPDATE,
                "Source shift changed; destination still matches the last synchronized values",
                "",
            )
        return (
            known,
            Outcome.CONFLICTED,
            "MitHF shift was manually changed; automatic update is blocked",
            "manually_changed",
        )

    exact = [
        item
        for item in candidates
        if item.starts_at.isoformat() == expected["starts_at"]
        and item.ends_at.isoformat() == expected["ends_at"]
        and item.helper_count == expected["helper_count"]
    ]
    if len(exact) == 1:
        return (
            exact[0],
            Outcome.ALREADY_MATCHED,
            "Existing MitHF shift matches by date, time, and helper count",
            "",
        )
    if len(exact) > 1:
        return (
            None,
            Outcome.CONFLICTED,
            "Multiple MitHF shifts match; destination identity is not unique: "
            + _describe_shifts(exact),
            "not_unique",
        )
    if record and record.status == "uncertain":
        return (
            None,
            Outcome.CONFLICTED,
            "Previous MitHF request has an uncertain outcome; manual reconciliation required",
            "uncertain_write",
        )
    overlapping = [
        item
        for item in candidates
        if item.starts_at < datetime.fromisoformat(expected["ends_at"])
        and item.ends_at > datetime.fromisoformat(expected["starts_at"])
    ]
    if overlapping:
        return (
            None,
            Outcome.CONFLICTED,
            "Existing MitHF shifts overlap the source interval "
            f"{expected['starts_at']} to {expected['ends_at']}; review split or "
            "changed shifts before creating another: " + _describe_shifts(overlapping),
            "overlapping",
        )
    return None, Outcome.WOULD_CREATE, "Would create the MitHF shift", ""


def _describe_shifts(shifts: Iterable[MitHfShift]) -> str:
    """Name the destination shifts behind a conflict that has no single match.

    These conflicts report no destination_id, so without the candidates there is
    nothing in the report to look up in MitHF.
    """
    return "; ".join(
        f"{shift.id} {shift.starts_at.isoformat()} to {shift.ends_at.isoformat()}"
        + (f" ({shift.helper_name})" if shift.helper_name else "")
        for shift in sorted(shifts, key=lambda shift: shift.starts_at)
    )


def _reconcile_duos(
    expected: dict[str, Any],
    record: StepRecord | None,
    candidates: tuple[DuosRegistration, ...],
) -> tuple[DuosRegistration | None, Outcome, str, str]:
    if record and record.destination_id:
        known = next(
            (item for item in candidates if item.id == record.destination_id), None
        )
        if known is None:
            return (
                None,
                Outcome.CONFLICTED,
                "Previously synchronized DUOS registration is missing; no replacement will be added",
                "destination_missing",
            )
        actual = _duos_payload(known)
        if actual == expected:
            if known.status_id in {2, 5, 6}:
                return (
                    known,
                    Outcome.REVIEW,
                    "DUOS registration is rejected, withdrawn, or offsetting",
                    "rejected",
                )
            return (
                known,
                Outcome.ALREADY_MATCHED,
                "DUOS registration already matches",
                "",
            )
        if actual == record.synced_payload:
            if known.status_id != 0:
                return (
                    known,
                    Outcome.REVIEW,
                    "DUOS registration is no longer pending; review changes manually",
                    "not_pending",
                )
            return (
                known,
                Outcome.WOULD_UPDATE,
                "SPS source changed; DUOS still matches the last synchronized values",
                "",
            )
        return (
            known,
            Outcome.CONFLICTED,
            "DUOS registration was manually changed; automatic update is blocked",
            "manually_changed",
        )

    exact = [item for item in candidates if _duos_payload(item) == expected]
    if len(exact) == 1:
        if exact[0].status_id in {2, 5, 6}:
            return (
                exact[0],
                Outcome.REVIEW,
                "Matching DUOS registration is rejected, withdrawn, or offsetting",
                "rejected",
            )
        return (
            exact[0],
            Outcome.ALREADY_MATCHED,
            "Existing DUOS registration matches exactly",
            "",
        )
    if len(exact) > 1:
        return (
            None,
            Outcome.CONFLICTED,
            "Multiple DUOS registrations match; destination identity is not unique",
            "not_unique",
        )
    if record and record.status == "uncertain":
        return (
            None,
            Outcome.CONFLICTED,
            "Previous DUOS request has an uncertain outcome; manual reconciliation required",
            "uncertain_write",
        )
    if any(
        item.employee_number == expected["employee_number"]
        and item.arrangement_id == expected["arrangement_id"]
        and item.starts_at < datetime.fromisoformat(expected["ends_at"])
        and item.ends_at > datetime.fromisoformat(expected["starts_at"])
        for item in candidates
    ):
        return (
            None,
            Outcome.CONFLICTED,
            "Existing DUOS hours overlap this interval; review before creating another registration",
            "overlapping",
        )
    return None, Outcome.WOULD_CREATE, "Would create the DUOS registration", ""


def _duos_payload(item: DuosRegistration) -> dict[str, Any]:
    return {
        "arrangement_id": item.arrangement_id,
        "employee_number": item.employee_number,
        "registration_type": item.registration_type,
        "starts_at": item.starts_at.isoformat(),
        "ends_at": item.ends_at.isoformat(),
    }


def _interval_payload(interval: TimeInterval) -> dict[str, str]:
    return {
        "starts_at": interval.starts_at.isoformat(),
        "ends_at": interval.ends_at.isoformat(),
    }
