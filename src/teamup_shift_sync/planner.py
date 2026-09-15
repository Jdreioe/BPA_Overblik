from __future__ import annotations

from collections.abc import Iterable
from datetime import datetime
from typing import Any
from zoneinfo import ZoneInfo

from .models import (
    AppConfig,
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    Outcome,
    PlanItem,
    SourceShift,
    SyncPlan,
    TimeInterval,
)
from .parser import parse_sps_instructions
from .source_rules import is_reminder, meeting_item
from .state import StepRecord, SyncState


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
                )
            )
            continue
        items.extend(
            _plan_shift(config, shift, destination, now, timezone, state, live)
        )
    return SyncPlan(range_start, range_end, now, tuple(items))


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
            )
        )
        return items

    shift_payload = {
        "starts_at": shift.starts_at.isoformat(),
        "ends_at": shift.ends_at.isoformat(),
        "helper_count": config.default_helper_count,
    }
    create_step = state.get_step(shift.key, "mithf.create_shift")
    mithf_match, shift_outcome, shift_summary = _reconcile_mithf_shift(
        shift_payload, create_step, destination.mithf_shifts
    )
    items.append(
        PlanItem(
            shift.key,
            "mithf",
            "mithf.create_shift",
            shift_outcome,
            shift_summary,
            shift_payload,
            mithf_match.id if mithf_match else None,
        )
    )

    assignment_payload = {"helper_name": mapping.mithf_name}
    if mithf_match is None:
        assignment_outcome = (
            Outcome.CONFLICTED
            if shift_outcome == Outcome.CONFLICTED
            else Outcome.WOULD_CREATE
        )
        assignment_summary = (
            "Helper assignment is blocked by the shift conflict"
            if assignment_outcome == Outcome.CONFLICTED
            else f"Would assign {mapping.mithf_name} after creating the shift"
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
    assignment_record = state.get_step(shift.key, "mithf.assign_helper")
    if (
        assignment_record
        and assignment_record.status == "uncertain"
        and assignment_outcome != Outcome.ALREADY_MATCHED
    ):
        assignment_outcome = Outcome.CONFLICTED
        assignment_summary = "Previous helper assignment has an uncertain outcome; review before retrying"
    items.append(
        PlanItem(
            shift.key,
            "mithf",
            "mithf.assign_helper",
            assignment_outcome,
            assignment_summary,
            assignment_payload,
            mithf_match.id if mithf_match else None,
        )
    )

    sps_record = state.get_step(shift.key, "mithf.set_sps")
    if parsed.intervals:
        expected_intervals = tuple(item.interval for item in parsed.intervals)
        sps_payload = {
            "intervals": [_interval_payload(item) for item in expected_intervals]
        }
        actual_intervals = mithf_match.sps_intervals if mithf_match else ()
        actual_payload = {
            "intervals": [_interval_payload(item) for item in actual_intervals]
        }
        if any(issue.code != "duplicate_uni_interval" for issue in parsed.issues):
            sps_outcome = Outcome.REVIEW
            sps_summary = "MitHF SPS is blocked by unresolved source instructions"
        elif (
            shift_outcome == Outcome.CONFLICTED
            or assignment_outcome == Outcome.CONFLICTED
        ):
            sps_outcome = Outcome.CONFLICTED
            sps_summary = "MitHF SPS is blocked by the shift or helper conflict"
        elif mithf_match and sorted(
            actual_intervals, key=lambda item: item.starts_at
        ) == sorted(expected_intervals, key=lambda item: item.starts_at):
            sps_outcome = Outcome.ALREADY_MATCHED
            sps_summary = "Existing MitHF SPS intervals match exactly"
        elif sps_record and actual_payload != sps_record.synced_payload:
            sps_outcome = Outcome.CONFLICTED
            sps_summary = "MitHF SPS changed since synchronization; automatic replacement is blocked"
        elif actual_intervals and sps_record is None:
            sps_outcome = Outcome.REVIEW
            sps_summary = (
                "Existing MitHF SPS differs from the source; review before replacing it"
            )
        elif len(expected_intervals) > 1:
            sps_outcome = Outcome.PENDING_MITHF_BUG
            sps_summary = "Separate SPS intervals preserved; MitHF multiple-interval writes remain unverified"
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
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                "mithf.set_sps",
                sps_outcome,
                sps_summary,
                sps_payload,
                mithf_match.id if mithf_match else None,
            )
        )
    elif sps_record:
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                "mithf.set_sps",
                Outcome.REVIEW,
                "Previously synchronized SPS is no longer in the source; no removal planned",
                sps_record.synced_payload,
                sps_record.destination_id,
            )
        )

    meeting = meeting_item(shift.title, shift.key, shift.starts_at, shift.ends_at)
    if meeting:
        payload = {
            "intervals": [
                _interval_payload(TimeInterval(shift.starts_at, shift.ends_at))
            ]
        }
        existing = mithf_match.meeting_intervals if mithf_match else ()
        if (
            shift_outcome == Outcome.CONFLICTED
            or assignment_outcome == Outcome.CONFLICTED
        ):
            outcome = Outcome.CONFLICTED
        elif existing == (TimeInterval(shift.starts_at, shift.ends_at),):
            outcome = Outcome.ALREADY_MATCHED
        elif existing:
            outcome = Outcome.REVIEW
        else:
            outcome = Outcome.WOULD_CREATE if live else Outcome.PENDING_INTEGRATION
        meeting_record = state.get_step(shift.key, meeting.step_key)
        if (
            meeting_record
            and meeting_record.status == "uncertain"
            and outcome != Outcome.ALREADY_MATCHED
        ):
            outcome = Outcome.CONFLICTED
        items.append(
            PlanItem(
                shift.key,
                "mithf",
                meeting.step_key,
                outcome,
                "Vagtmøde for the full shift; existing differences require review",
                payload,
                mithf_match.id if mithf_match else None,
            )
        )

    expected_duos_steps: set[str] = set()
    parse_is_blocked = any(
        issue.code != "duplicate_uni_interval" for issue in parsed.issues
    )
    for sps in parsed.intervals:
        step_key = f"duos.interval:{sps.key}"
        expected_duos_steps.add(step_key)
        payload = {
            "arrangement_id": config.duos_arrangement_id,
            "employee_number": mapping.duos_employee_number,
            "registration_type": config.duos_registration_type,
            **_interval_payload(sps.interval),
        }
        if parse_is_blocked:
            items.append(
                PlanItem(
                    shift.key,
                    "duos",
                    step_key,
                    Outcome.REVIEW,
                    "DUOS write is blocked because another SPS instruction on this shift could not be resolved safely",
                    payload,
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
                )
            )
            continue
        record = state.get_step(shift.key, step_key)
        match, outcome, summary = _reconcile_duos(
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
            )
        )

    for record in state.steps_for_source(shift.key):
        if (
            record.step_key.startswith("duos.interval:")
            and record.step_key not in expected_duos_steps
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
                )
            )
    return items


def _reconcile_mithf_shift(
    expected: dict[str, Any],
    record: StepRecord | None,
    candidates: tuple[MitHfShift, ...],
) -> tuple[MitHfShift | None, Outcome, str]:
    if record and record.destination_id:
        known = next(
            (item for item in candidates if item.id == record.destination_id), None
        )
        if known is None:
            return (
                None,
                Outcome.CONFLICTED,
                "Previously synchronized MitHF shift is missing; it will not be recreated automatically",
            )
        actual = {
            "starts_at": known.starts_at.isoformat(),
            "ends_at": known.ends_at.isoformat(),
            "helper_count": known.helper_count,
        }
        if actual == expected:
            return known, Outcome.ALREADY_MATCHED, "MitHF shift already matches"
        if actual == record.synced_payload:
            return (
                known,
                Outcome.WOULD_UPDATE,
                "Source shift changed; destination still matches the last synchronized values",
            )
        return (
            known,
            Outcome.CONFLICTED,
            "MitHF shift was manually changed; automatic update is blocked",
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
        )
    if len(exact) > 1:
        return (
            None,
            Outcome.CONFLICTED,
            "Multiple MitHF shifts match; destination identity is not unique: "
            + _describe_shifts(exact),
        )
    if record and record.status == "uncertain":
        return (
            None,
            Outcome.CONFLICTED,
            "Previous MitHF request has an uncertain outcome; manual reconciliation required",
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
        )
    return None, Outcome.WOULD_CREATE, "Would create the MitHF shift"


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
) -> tuple[DuosRegistration | None, Outcome, str]:
    if record and record.destination_id:
        known = next(
            (item for item in candidates if item.id == record.destination_id), None
        )
        if known is None:
            return (
                None,
                Outcome.CONFLICTED,
                "Previously synchronized DUOS registration is missing; no replacement will be added",
            )
        actual = _duos_payload(known)
        if actual == expected:
            if known.status_id in {2, 5, 6}:
                return (
                    known,
                    Outcome.REVIEW,
                    "DUOS registration is rejected, withdrawn, or offsetting",
                )
            return known, Outcome.ALREADY_MATCHED, "DUOS registration already matches"
        if actual == record.synced_payload:
            if known.status_id != 0:
                return (
                    known,
                    Outcome.REVIEW,
                    "DUOS registration is no longer pending; review changes manually",
                )
            return (
                known,
                Outcome.WOULD_UPDATE,
                "SPS source changed; DUOS still matches the last synchronized values",
            )
        return (
            known,
            Outcome.CONFLICTED,
            "DUOS registration was manually changed; automatic update is blocked",
        )

    exact = [item for item in candidates if _duos_payload(item) == expected]
    if len(exact) == 1:
        if exact[0].status_id in {2, 5, 6}:
            return (
                exact[0],
                Outcome.REVIEW,
                "Matching DUOS registration is rejected, withdrawn, or offsetting",
            )
        return (
            exact[0],
            Outcome.ALREADY_MATCHED,
            "Existing DUOS registration matches exactly",
        )
    if len(exact) > 1:
        return (
            None,
            Outcome.CONFLICTED,
            "Multiple DUOS registrations match; destination identity is not unique",
        )
    if record and record.status == "uncertain":
        return (
            None,
            Outcome.CONFLICTED,
            "Previous DUOS request has an uncertain outcome; manual reconciliation required",
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
        )
    return None, Outcome.WOULD_CREATE, "Would create the DUOS registration"


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
