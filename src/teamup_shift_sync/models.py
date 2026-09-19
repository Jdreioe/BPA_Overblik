from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from enum import StrEnum
from typing import Any


@dataclass(frozen=True)
class SourceComment:
    id: str
    text: str
    updated_at: datetime | None = None


@dataclass(frozen=True)
class SourceShift:
    calendar_id: str
    event_id: str
    occurrence_id: str
    title: str
    helper_key: str
    starts_at: datetime
    ends_at: datetime
    #: The TeamUp event description, where SPS instructions are normally written.
    notes: str = ""
    comments: tuple[SourceComment, ...] = ()
    recurrence_start: datetime | None = None
    source_version: str | None = None

    @property
    def key(self) -> str:
        return f"{self.calendar_id}:{self.event_id}:{self.occurrence_id}"


@dataclass(frozen=True)
class TimeInterval:
    starts_at: datetime
    ends_at: datetime

    @property
    def hours(self) -> float:
        elapsed = self.ends_at.timestamp() - self.starts_at.timestamp()
        return elapsed / 3600


@dataclass(frozen=True)
class SpsInterval:
    key: str
    #: Where the instruction was written: NOTES_SOURCE_ID or a TeamUp comment id.
    source_id: str
    ordinal: int
    interval: TimeInterval


@dataclass(frozen=True)
class ParseIssue:
    code: str
    message: str
    source_id: str


@dataclass(frozen=True)
class SpsParseResult:
    intervals: tuple[SpsInterval, ...] = ()
    issues: tuple[ParseIssue, ...] = ()
    relevant_source_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class HelperMapping:
    teamup_key: str
    teamup_display_name: str
    mithf_name: str
    duos_name: str
    duos_employee_number: str
    mithf_id: str = ""


@dataclass(frozen=True)
class AppConfig:
    timezone: str
    default_helper_count: int
    teamup_calendar_key: str
    teamup_api_key: str
    teamup_bearer_token: str
    teamup_helper_field: str
    duos_arrangement_id: str
    duos_registration_type: str
    helpers: dict[str, HelperMapping]
    teamup_subcalendar_helpers: dict[str, str] = field(default_factory=dict)
    teamup_lookback_days: int = 7
    mithf_customer_id: str = ""
    mithf_grant_id: str = ""


@dataclass(frozen=True)
class TeamUpOccurrence:
    id: str
    series_id: str
    title: str
    starts_at: datetime
    ends_at: datetime
    all_day: bool
    notes: str
    comments: tuple[SourceComment, ...]
    recurrence_start: datetime | None
    version: str | None
    raw: dict[str, Any] = field(repr=False, compare=False)


@dataclass(frozen=True)
class MitHfShift:
    id: str
    starts_at: datetime
    ends_at: datetime
    helper_count: int
    helper_name: str | None
    sps_intervals: tuple[TimeInterval, ...] = ()
    meeting_intervals: tuple[TimeInterval, ...] = ()
    sps_record_ids: tuple[str, ...] = ()
    meeting_record_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class DuosRegistration:
    id: str
    arrangement_id: str
    employee_number: str
    registration_type: str
    starts_at: datetime
    ends_at: datetime
    status_id: int = 0


@dataclass(frozen=True)
class DestinationSnapshot:
    mithf_shifts: tuple[MitHfShift, ...] = ()
    duos_registrations: tuple[DuosRegistration, ...] = ()


class Outcome(StrEnum):
    WOULD_CREATE = "would_create"
    WOULD_UPDATE = "would_update"
    ALREADY_MATCHED = "already_matched"
    CONFLICTED = "conflicted"
    FAILED = "failed"
    REVIEW = "review"
    EXCLUDED = "excluded"
    PENDING_INTEGRATION = "pending_integration"
    PENDING_MITHF_BUG = "pending_mithf_bug"


@dataclass(frozen=True)
class PlanItem:
    source_key: str
    system: str
    step_key: str
    outcome: Outcome
    summary: str
    payload: dict[str, Any] = field(default_factory=dict)
    destination_id: str | None = None


@dataclass(frozen=True)
class SyncPlan:
    starts_at: datetime
    ends_at: datetime
    generated_at: datetime
    items: tuple[PlanItem, ...]
