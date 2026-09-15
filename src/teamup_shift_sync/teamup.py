from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Mapping
from datetime import UTC, date, datetime, time, timedelta
from typing import Any, Protocol
from zoneinfo import ZoneInfo

from .models import AppConfig, SourceComment, SourceShift, TeamUpOccurrence


class TeamUpError(RuntimeError):
    """A safe-to-display TeamUp integration error."""


class JsonTransport(Protocol):
    def get_json(
        self, url: str, *, headers: Mapping[str, str], params: Mapping[str, str]
    ) -> dict[str, Any]: ...


class UrllibJsonTransport:
    def __init__(self, timeout_seconds: float = 20):
        self.timeout_seconds = timeout_seconds

    def get_json(
        self, url: str, *, headers: Mapping[str, str], params: Mapping[str, str]
    ) -> dict[str, Any]:
        request_url = f"{url}?{urllib.parse.urlencode(params)}" if params else url
        request = urllib.request.Request(
            request_url, headers=dict(headers), method="GET"
        )
        try:
            with urllib.request.urlopen(
                request, timeout=self.timeout_seconds
            ) as response:
                body = response.read()
        except urllib.error.HTTPError as error:
            # TeamUp error bodies may contain account details. Do not echo them.
            raise TeamUpError(f"TeamUp returned HTTP {error.code}") from error
        except urllib.error.URLError as error:
            raise TeamUpError(f"Could not reach TeamUp: {error.reason}") from error
        try:
            decoded = json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise TeamUpError("TeamUp returned an invalid JSON response") from error
        if not isinstance(decoded, dict):
            raise TeamUpError("TeamUp returned an unexpected JSON response")
        return decoded


class TeamUpClient:
    base_url = "https://api.teamup.com"

    def __init__(self, config: AppConfig, transport: JsonTransport | None = None):
        if not config.teamup_calendar_key:
            raise TeamUpError(
                "TeamUp calendar_key is required (set TEAMUP_CALENDAR_KEY or teamup.calendar_key)"
            )
        if not config.teamup_api_key:
            raise TeamUpError(
                "TeamUp api_key is required (set TEAMUP_API or teamup.api_key)"
            )
        self.calendar_key = config.teamup_calendar_key
        # Calendar links grant access: never put the link key in reports or state IDs.
        self.calendar_id = hashlib.sha256(self.calendar_key.encode()).hexdigest()[:24]
        self.subcalendar_helpers = config.teamup_subcalendar_helpers
        self.timezone = ZoneInfo(config.timezone)
        self.lookback_days = config.teamup_lookback_days
        self.transport = transport or UrllibJsonTransport()
        self.headers = {
            "Accept": "application/json",
            "Teamup-Token": config.teamup_api_key,
            "User-Agent": "teamup-shift-sync/0.1",
        }
        if config.teamup_bearer_token:
            self.headers["Authorization"] = f"Bearer {config.teamup_bearer_token}"

    def fetch_occurrences(self, start: date, end: date) -> tuple[TeamUpOccurrence, ...]:
        """Fetch an inclusive local date range and hydrate each event's comments.

        Include a configurable lookback, then filter by interval overlap. The
        lookback must cover the longest supported source shift.

        TeamUp's normal date-range endpoint does not document pagination. The
        search endpoint has offset/limit pagination, but it is a different API
        and is intentionally not used for complete weekly reads.
        """
        if end < start:
            raise TeamUpError("TeamUp end date must be on or after the start date")
        range_start = datetime.combine(start, time.min, self.timezone)
        range_end = datetime.combine(end + timedelta(days=1), time.min, self.timezone)
        response = self._get(
            "/events",
            {
                "startDate": (start - timedelta(days=self.lookback_days)).isoformat(),
                "endDate": end.isoformat(),
                "tz": self.timezone.key,
                "format": "markdown",
            },
        )
        raw_events = response.get("events")
        if not isinstance(raw_events, list):
            raise TeamUpError("TeamUp event response did not contain an events list")

        occurrences: list[TeamUpOccurrence] = []
        for summary in raw_events:
            if not isinstance(summary, dict) or not summary.get("id"):
                raise TeamUpError("TeamUp returned an event without a stable id")
            # The official single-event endpoint includes auxiliary data such as
            # comments. Reading it explicitly avoids confusing notes with comments.
            detail_response = self._get(
                f"/events/{urllib.parse.quote(str(summary['id']), safe='-')}",
                {"format": "markdown"},
            )
            detail = detail_response.get("event")
            if not isinstance(detail, dict):
                raise TeamUpError("TeamUp event detail response was missing the event")
            occurrence = self._occurrence(detail)
            if occurrence.ends_at > range_start and occurrence.starts_at < range_end:
                occurrences.append(occurrence)
        return tuple(occurrences)

    def to_source_shift(
        self, occurrence: TeamUpOccurrence, helper_field: str
    ) -> SourceShift:
        if occurrence.all_day:
            raise TeamUpError(
                f"All-day TeamUp event {occurrence.id} cannot be imported as a timed shift"
            )
        if helper_field == "subcalendar":
            ids = occurrence.raw.get("subcalendar_ids")
            if not isinstance(ids, list):
                raise TeamUpError("Event is missing its subcalendar assignment")
            matches = {str(value) for value in ids} & self.subcalendar_helpers.keys()
            if len(matches) != 1:
                raise TeamUpError(
                    "Event must belong to exactly one configured helper subcalendar; "
                    f"found {len(matches)}"
                )
            helper_key = next(iter(matches))
        else:
            helper_key = _helper_key(occurrence.raw, helper_field)
        return SourceShift(
            calendar_id=self.calendar_id,
            event_id=occurrence.series_id,
            occurrence_id=occurrence.id,
            title=occurrence.title,
            helper_key=helper_key,
            starts_at=occurrence.starts_at,
            ends_at=occurrence.ends_at,
            notes=occurrence.notes,
            comments=occurrence.comments,
            recurrence_start=occurrence.recurrence_start,
            source_version=occurrence.version,
        )

    def _get(self, suffix: str, params: Mapping[str, str]) -> dict[str, Any]:
        calendar = urllib.parse.quote(self.calendar_key, safe="")
        return self.transport.get_json(
            f"{self.base_url}/{calendar}{suffix}", headers=self.headers, params=params
        )

    def _occurrence(self, raw: dict[str, Any]) -> TeamUpOccurrence:
        occurrence_id = str(_required(raw, "id"))
        series_id = str(raw.get("series_id") or occurrence_id.split("-rid-", 1)[0])
        comments_enabled = bool(raw.get("comments_enabled", False))
        raw_comments = raw.get("comments")
        comments_may_be_hidden = (
            comments_enabled
            and raw.get("comments_visibility") == "users_with_modify_permission"
            and bool(raw.get("readonly", True))
            and not raw_comments
        )
        if comments_may_be_hidden:
            raise TeamUpError(
                f"Comments on TeamUp event {occurrence_id} may be hidden by modify-only visibility"
            )
        if comments_enabled and raw_comments is None:
            raise TeamUpError(
                f"Comments are enabled on TeamUp event {occurrence_id} but were not readable"
            )
        if raw_comments is None:
            raw_comments = []
        if not isinstance(raw_comments, list):
            raise TeamUpError(f"TeamUp event {occurrence_id} returned invalid comments")
        comments = tuple(_comment(item, self.timezone) for item in raw_comments)
        starts_at = _teamup_datetime(_required(raw, "start_dt"), self.timezone)
        ends_at = _teamup_datetime(_required(raw, "end_dt"), self.timezone)
        if ends_at <= starts_at:
            raise TeamUpError(f"TeamUp event {occurrence_id} has an invalid duration")
        return TeamUpOccurrence(
            id=occurrence_id,
            series_id=series_id,
            title=str(raw.get("title") or ""),
            starts_at=starts_at,
            ends_at=ends_at,
            all_day=bool(raw.get("all_day", False)),
            notes=str(raw.get("notes") or ""),
            comments=comments,
            recurrence_start=(
                _teamup_datetime(raw["ristart_dt"], self.timezone)
                if raw.get("ristart_dt") is not None
                else None
            ),
            version=str(raw["version"]) if raw.get("version") is not None else None,
            raw=raw,
        )


def _helper_key(raw: dict[str, Any], field: str) -> str:
    if field in {"who", "title"}:
        value = raw.get(field)
    elif field.startswith("custom.") and len(field) > len("custom."):
        custom = raw.get("custom")
        value = (
            custom.get(field.removeprefix("custom."))
            if isinstance(custom, dict)
            else None
        )
    else:
        raise TeamUpError(
            "teamup.helper_field must be explicitly set to who, title, or custom.<field-id>"
        )
    if not isinstance(value, str) or not value.strip():
        raise TeamUpError(f"Configured helper field {field!r} is empty or not text")
    return value.strip()


def _comment(raw: Any, timezone: ZoneInfo) -> SourceComment:
    if not isinstance(raw, dict):
        raise TeamUpError("TeamUp returned an invalid comment")
    message = raw.get("message")
    if isinstance(message, dict):
        text = message.get("markdown")
        if not isinstance(text, str):
            raise TeamUpError("TeamUp did not return a Markdown version of a comment")
    elif isinstance(message, str):
        text = message
    else:
        raise TeamUpError("TeamUp returned a comment without text")
    updated = raw.get("update_dt") or raw.get("creation_dt")
    return SourceComment(
        id=str(_required(raw, "id")),
        text=text,
        updated_at=_teamup_datetime(updated, timezone) if updated is not None else None,
    )


def _teamup_datetime(value: Any, timezone: ZoneInfo) -> datetime:
    if isinstance(value, bool):
        raise TeamUpError("TeamUp returned an invalid datetime")
    if isinstance(value, int | float):
        return datetime.fromtimestamp(value, UTC).astimezone(timezone)
    if not isinstance(value, str):
        raise TeamUpError("TeamUp returned an invalid datetime")
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError as error:
        raise TeamUpError(f"TeamUp returned an invalid datetime: {value!r}") from error
    if parsed.tzinfo is None:
        candidates: dict[datetime, datetime] = {}
        for fold in (0, 1):
            aware = parsed.replace(tzinfo=timezone, fold=fold)
            round_trip = aware.astimezone(UTC).astimezone(timezone).replace(tzinfo=None)
            if round_trip == parsed:
                candidates[aware.astimezone(UTC)] = aware
        if not candidates:
            raise TeamUpError(
                f"TeamUp returned a nonexistent local datetime: {value!r}"
            )
        if len(candidates) > 1:
            raise TeamUpError(f"TeamUp returned an ambiguous local datetime: {value!r}")
        parsed = next(iter(candidates.values()))
    return parsed


def _required(raw: dict[str, Any], key: str) -> Any:
    value = raw.get(key)
    if value is None:
        raise TeamUpError(f"TeamUp response is missing {key}")
    return value
