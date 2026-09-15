from __future__ import annotations

from datetime import datetime

from teamup_shift_sync.models import (
    AppConfig,
    HelperMapping,
    SourceComment,
    SourceShift,
)


def moment(value: str) -> datetime:
    return datetime.fromisoformat(value)


def source_shift(
    *,
    starts_at: str = "2026-09-14T07:30:00+02:00",
    ends_at: str = "2026-09-14T15:00:00+02:00",
    comment: str | None = "uni 8-10 & 13-14",
    comment_id: str = "comment-1",
    notes: str = "",
) -> SourceShift:
    comments = () if comment is None else (SourceComment(comment_id, comment),)
    return SourceShift(
        calendar_id="calendar",
        event_id="event",
        occurrence_id=starts_at,
        title="Shift",
        helper_key="helper",
        starts_at=moment(starts_at),
        ends_at=moment(ends_at),
        notes=notes,
        comments=comments,
    )


def config() -> AppConfig:
    mapping = HelperMapping("helper", "Helper", "Mit Helper", "DUOS Helper", "123")
    return AppConfig(
        timezone="Europe/Copenhagen",
        default_helper_count=1,
        teamup_calendar_key="calendar",
        teamup_api_key="fixture",
        teamup_bearer_token="",
        teamup_helper_field="who",
        duos_arrangement_id="arrangement",
        duos_registration_type="Almindelig",
        helpers={"helper": mapping},
    )
