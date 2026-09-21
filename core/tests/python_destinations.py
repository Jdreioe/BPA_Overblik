"""Temporary parity oracle for the Rust live write adapter, synthetic data only.

Drives the current Python ``Destinations.write`` through a recording transport
and prints the exact request each planned operation submits. No network or
browser is involved, and no real account is referenced.
"""

import json
import sys
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).parents[2] / "src"))

from teamup_shift_sync.destinations import Destinations
from teamup_shift_sync.models import (
    AppConfig,
    DestinationSnapshot,
    HelperMapping,
    MitHfShift,
    PlanItem,
)

ZONE = ZoneInfo("Europe/Copenhagen")
HELPERS = {"Anna Hansen": "va-1", "Bo Jensen": "va-2"}
CREATION = {"klient": "k-7", "bevilling": "b-3"}


def moment(text: str) -> datetime:
    return datetime.fromisoformat(text).astimezone(ZONE)


def adapter() -> Destinations:
    config = AppConfig(
        timezone="Europe/Copenhagen",
        default_helper_count=1,
        teamup_calendar_key="",
        teamup_api_key="",
        teamup_bearer_token="",
        teamup_helper_field="",
        duos_arrangement_id="55",
        duos_registration_type="4",
        helpers={
            "cal-1": HelperMapping(
                teamup_key="cal-1",
                teamup_display_name="Anna",
                mithf_name="Anna Hansen",
                duos_name="Anna Hansen",
                duos_employee_number="801",
            )
        },
    )
    destinations = Destinations(Recorder(), config)
    destinations.helpers = dict(HELPERS)
    destinations.creation_options = dict(CREATION)
    return destinations


class Recorder:
    """Accepts every call, records it, and answers the minimum each step needs."""

    def __init__(self) -> None:
        self.calls: list[dict] = []

    def request(self, system: str, action: str, payload: dict):
        self.calls.append({"system": system, "action": action, "payload": payload})
        if system == "duos" and action == "detail":
            return {"status": "Afventer"}
        if system == "mithf" and action == "opret":
            return {"ok": True, "eid": "9001"}
        return {"ok": True}


def shift(identifier: str, start: str, end: str, **extra) -> MitHfShift:
    return MitHfShift(
        id=identifier,
        starts_at=moment(start),
        ends_at=moment(end),
        helper_count=1,
        helper_name=extra.pop("helper_name", None),
        **extra,
    )


def duos_item(start: str, end: str, destination_id: str | None = None) -> PlanItem:
    return PlanItem(
        source_key="cal-1:ev-1:oc-1",
        system="duos",
        step_key="duos.interval:1",
        outcome="would_create",
        summary="",
        payload={
            "arrangement_id": "55",
            "employee_number": "801",
            "registration_type": "4",
            "starts_at": moment(start).isoformat(),
            "ends_at": moment(end).isoformat(),
        },
        destination_id=destination_id,
    )


def mithf_item(step_key: str, payload: dict, destination_id: str | None = None) -> PlanItem:
    return PlanItem(
        source_key="cal-1:ev-1:oc-1",
        system="mithf",
        step_key=step_key,
        outcome="would_create",
        summary="",
        payload=payload,
        destination_id=destination_id,
    )


def times(start: str, end: str, **extra) -> dict:
    return {"starts_at": moment(start).isoformat(), "ends_at": moment(end).isoformat(), **extra}


def intervals(start: str, end: str) -> dict:
    return {"intervals": [times(start, end)]}


EXISTING = shift("7001", "2024-03-04T08:00+01:00", "2024-03-04T16:00+01:00")
OVERNIGHT = shift("7002", "2024-03-04T22:00+01:00", "2024-03-05T06:00+01:00")
WITH_SPS = shift(
    "7003",
    "2024-03-04T08:00+01:00",
    "2024-03-04T16:00+01:00",
    sps_record_ids=("r-11",),
)

CASES = [
    ("duos_create", duos_item("2024-03-04T08:00+01:00", "2024-03-04T16:00+01:00"), DestinationSnapshot()),
    ("duos_update", duos_item("2024-03-04T08:00+01:00", "2024-03-04T16:00+01:00", "5501"), DestinationSnapshot()),
    # Ends on the winter clock change: the two offsets must differ in the snapshot.
    ("duos_dst_end", duos_item("2024-10-27T01:00+02:00", "2024-10-27T04:00+01:00"), DestinationSnapshot()),
    ("duos_dst_start", duos_item("2024-03-31T01:00+01:00", "2024-03-31T04:00+02:00"), DestinationSnapshot()),
    (
        "mithf_create",
        mithf_item("mithf.create_shift", times("2024-03-04T08:00+01:00", "2024-03-04T16:00+01:00", helper_count=1)),
        DestinationSnapshot(),
    ),
    (
        "mithf_create_overnight",
        mithf_item("mithf.create_shift#1", times("2024-03-04T22:00+01:00", "2024-03-05T06:00+01:00", helper_count=1)),
        DestinationSnapshot(),
    ),
    (
        "mithf_update_time",
        mithf_item(
            "mithf.create_shift",
            times("2024-03-04T09:00+01:00", "2024-03-04T17:00+01:00", helper_count=1),
            EXISTING.id,
        ),
        DestinationSnapshot(mithf_shifts=(EXISTING,)),
    ),
    (
        "mithf_assign_helper",
        mithf_item("mithf.assign_helper", {"helper_name": "Anna Hansen"}, EXISTING.id),
        DestinationSnapshot(mithf_shifts=(EXISTING,)),
    ),
    (
        "mithf_assign_helper_overnight",
        mithf_item("mithf.assign_helper#2", {"helper_name": "Bo Jensen"}, OVERNIGHT.id),
        DestinationSnapshot(mithf_shifts=(OVERNIGHT,)),
    ),
    (
        "mithf_sps_create",
        mithf_item("mithf.set_sps", intervals("2024-03-04T10:00+01:00", "2024-03-04T12:00+01:00"), EXISTING.id),
        DestinationSnapshot(mithf_shifts=(EXISTING,)),
    ),
    (
        "mithf_sps_create_overnight",
        mithf_item("mithf.set_sps#1", intervals("2024-03-04T23:00+01:00", "2024-03-05T02:00+01:00"), OVERNIGHT.id),
        DestinationSnapshot(mithf_shifts=(OVERNIGHT,)),
    ),
    (
        "mithf_sps_update",
        mithf_item("mithf.set_sps", intervals("2024-03-04T11:00+01:00", "2024-03-04T13:00+01:00"), WITH_SPS.id),
        DestinationSnapshot(mithf_shifts=(WITH_SPS,)),
    ),
]


def main() -> None:
    # Fixed so the DUOS completed-interval rule never depends on the clock.
    now = moment("2025-01-01T00:00+01:00")
    output = []
    for name, item, snapshot in CASES:
        destinations = adapter()
        destinations.write(item, snapshot)
        calls = [c for c in destinations.transport.calls if c["action"] != "detail"]
        assert len(calls) == 1, name
        output.append(
            {
                "name": name,
                "item": {
                    "source_key": item.source_key,
                    "system": item.system,
                    "step_key": item.step_key,
                    "outcome": str(item.outcome),
                    "summary": item.summary,
                    "payload": item.payload,
                    "destination_id": item.destination_id,
                    "reason": item.reason,
                },
                "snapshot": {
                    "mithf_shifts": [
                        {
                            "id": s.id,
                            "starts_at": s.starts_at.isoformat(),
                            "ends_at": s.ends_at.isoformat(),
                            "helper_count": s.helper_count,
                            "helper_name": s.helper_name,
                            "sps_intervals": [],
                            "meeting_intervals": [],
                            "sps_record_ids": list(s.sps_record_ids),
                            "meeting_record_ids": list(s.meeting_record_ids),
                        }
                        for s in snapshot.mithf_shifts
                    ],
                    "duos_registrations": [],
                },
                "now": now.isoformat(),
                "request": calls[0],
            }
        )
    json.dump(output, sys.stdout)


if __name__ == "__main__":
    main()
