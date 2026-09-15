from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path
from typing import Any

from .models import (
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    SourceComment,
    SourceShift,
    TimeInterval,
)


class FixtureError(ValueError):
    pass


def load_fixture(path: Path) -> tuple[tuple[SourceShift, ...], DestinationSnapshot]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise FixtureError(f"Fixture not found: {path}") from error
    except json.JSONDecodeError as error:
        raise FixtureError(f"Invalid JSON in {path}: {error}") from error

    try:
        shifts = tuple(_source_shift(item) for item in data.get("source_shifts", []))
        snapshot = DestinationSnapshot(
            mithf_shifts=tuple(
                _mithf_shift(item) for item in data.get("mithf_shifts", [])
            ),
            duos_registrations=tuple(
                _duos_registration(item) for item in data.get("duos_registrations", [])
            ),
        )
    except (KeyError, TypeError, ValueError) as error:
        raise FixtureError(f"Invalid fixture data in {path}: {error}") from error
    return shifts, snapshot


def _datetime(value: str) -> datetime:
    parsed = datetime.fromisoformat(value)
    if parsed.tzinfo is None:
        raise ValueError(f"Datetime must include an offset: {value}")
    return parsed


def _source_shift(raw: dict[str, Any]) -> SourceShift:
    comments = tuple(
        SourceComment(
            id=item["id"],
            text=item["text"],
            updated_at=_datetime(item["updated_at"])
            if item.get("updated_at")
            else None,
        )
        for item in raw.get("comments", [])
    )
    shift = SourceShift(
        calendar_id=raw["calendar_id"],
        event_id=raw["event_id"],
        occurrence_id=raw["occurrence_id"],
        title=raw["title"],
        helper_key=raw["helper_key"],
        starts_at=_datetime(raw["starts_at"]),
        ends_at=_datetime(raw["ends_at"]),
        comments=comments,
    )
    if shift.ends_at <= shift.starts_at:
        raise ValueError(f"Shift ends before it starts: {shift.key}")
    return shift


def _interval(raw: dict[str, Any]) -> TimeInterval:
    return TimeInterval(_datetime(raw["starts_at"]), _datetime(raw["ends_at"]))


def _mithf_shift(raw: dict[str, Any]) -> MitHfShift:
    return MitHfShift(
        id=raw["id"],
        starts_at=_datetime(raw["starts_at"]),
        ends_at=_datetime(raw["ends_at"]),
        helper_count=int(raw["helper_count"]),
        helper_name=raw.get("helper_name"),
        sps_intervals=tuple(_interval(item) for item in raw.get("sps_intervals", [])),
    )


def _duos_registration(raw: dict[str, Any]) -> DuosRegistration:
    return DuosRegistration(
        id=raw["id"],
        arrangement_id=raw["arrangement_id"],
        employee_number=raw["employee_number"],
        registration_type=raw["registration_type"],
        starts_at=_datetime(raw["starts_at"]),
        ends_at=_datetime(raw["ends_at"]),
    )
