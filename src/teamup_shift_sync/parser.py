from __future__ import annotations

import re
from datetime import UTC, date, datetime, time, timedelta
from zoneinfo import ZoneInfo

from .models import ParseIssue, SourceShift, SpsInterval, SpsParseResult, TimeInterval

#: Identity used for instructions written in the event description rather than
#: a comment. Comments carry TeamUp ids; the description has none.
NOTES_SOURCE_ID = "notes"

#: Accepted Danish weekday spellings: full names, common abbreviations, and
#: the plain-vowel forms typed without ø. Longest first so the alternation
#: never matches a prefix of a longer spelling.
_WEEKDAYS = {
    spelling: index
    for index, spellings in enumerate(
        (
            ("mandag", "man"),
            ("tirsdag", "tirs", "tir"),
            ("onsdag", "ons"),
            ("torsdag", "tors", "tor"),
            ("fredag", "fre"),
            ("lørdag", "lordag", "lør", "lor"),
            ("søndag", "sondag", "søn", "son"),
        )
    )
    for spelling in spellings
}
_WEEKDAY_PATTERN = "|".join(sorted(_WEEKDAYS, key=len, reverse=True))
_UNI_LINE = re.compile(
    r"^\s*uni(?:\s+(?P<day>\d{4}-\d{2}-\d{2}))?\s+(?P<body>.+?)"
    rf"(?:\s+(?P<weekday>{_WEEKDAY_PATTERN})\.?)?\s*$",
    re.IGNORECASE,
)
_INTERVAL = re.compile(
    r"(?P<start_hour>[01]?\d|2[0-3])"
    r"(?::(?P<start_minute>[0-5]\d))?\s*[-–]\s*"
    r"(?P<end_hour>[01]?\d|2[0-3])"
    r"(?::(?P<end_minute>[0-5]\d))?"
)


def parse_sps_instructions(shift: SourceShift, timezone: ZoneInfo) -> SpsParseResult:
    intervals: list[SpsInterval] = []
    issues: list[ParseIssue] = []
    relevant: list[str] = []
    local_start = shift.starts_at.astimezone(timezone)
    local_end_inclusive = (shift.ends_at - timedelta(microseconds=1)).astimezone(
        timezone
    )
    implicit_day = (
        local_start.date() if local_start.date() == local_end_inclusive.date() else None
    )

    blocks = [(NOTES_SOURCE_ID, shift.notes)]
    blocks.extend((comment.id, comment.text) for comment in shift.comments)
    for source_id, text in blocks:
        lines = [
            line
            for line in text.splitlines()
            if re.search(r"\buni\b", line, re.IGNORECASE)
        ]
        if not lines:
            continue
        relevant.append(source_id)
        block_ordinal = 0
        for line_number, line in enumerate(lines, start=1):
            match = _UNI_LINE.fullmatch(line)
            if not match:
                issues.append(
                    ParseIssue(
                        "unsupported_uni_syntax",
                        f"{source_id} line {line_number} contains 'uni' but does not match the supported syntax",
                        source_id,
                    )
                )
                continue

            explicit_day = match.group("day")
            weekday = match.group("weekday")
            if explicit_day:
                try:
                    interval_day = date.fromisoformat(explicit_day)
                except ValueError:
                    issues.append(
                        ParseIssue(
                            "invalid_uni_date",
                            f"Invalid SPS date: {explicit_day}",
                            source_id,
                        )
                    )
                    continue
            elif weekday:
                offset = (_WEEKDAYS[weekday.casefold()] - local_start.weekday()) % 7
                interval_day = local_start.date() + timedelta(days=offset)
                if interval_day > local_end_inclusive.date():
                    issues.append(
                        ParseIssue(
                            "uni_weekday_outside_shift",
                            "The named weekday is outside the source shift",
                            source_id,
                        )
                    )
                    continue
                if interval_day + timedelta(days=7) <= local_end_inclusive.date():
                    issues.append(
                        ParseIssue(
                            "ambiguous_uni_weekday",
                            "The shift contains this weekday more than once; use an ISO date",
                            source_id,
                        )
                    )
                    continue
            elif implicit_day is not None:
                interval_day = implicit_day
            else:
                issues.append(
                    ParseIssue(
                        "ambiguous_uni_date",
                        "An undated SPS instruction belongs to a shift spanning more than one local date",
                        source_id,
                    )
                )
                continue

            if weekday and interval_day.weekday() != _WEEKDAYS[weekday.casefold()]:
                issues.append(
                    ParseIssue(
                        "conflicting_uni_date",
                        "The explicit date and weekday disagree",
                        source_id,
                    )
                )
                continue

            tokens = re.split(r"\s*&\s*", match.group("body"))
            for token in tokens:
                ordinal = block_ordinal
                block_ordinal += 1
                parsed = _INTERVAL.fullmatch(token.strip())
                if not parsed:
                    issues.append(
                        ParseIssue(
                            "invalid_uni_interval",
                            f"Unsupported SPS interval: {token.strip()!r}",
                            source_id,
                        )
                    )
                    continue
                start_clock = time(
                    int(parsed.group("start_hour")),
                    int(parsed.group("start_minute") or 0),
                )
                end_clock = time(
                    int(parsed.group("end_hour")), int(parsed.group("end_minute") or 0)
                )
                end_day = (
                    interval_day + timedelta(days=1)
                    if end_clock <= start_clock
                    else interval_day
                )
                start, start_problem = _localize(interval_day, start_clock, timezone)
                end, end_problem = _localize(end_day, end_clock, timezone)
                if start_problem or end_problem:
                    issue = start_problem or end_problem
                    issues.append(
                        ParseIssue(
                            f"{issue}_local_time",
                            f"SPS interval {token.strip()!r} contains an {issue} local time in {timezone.key}",
                            source_id,
                        )
                    )
                    continue
                assert start is not None and end is not None
                interval = TimeInterval(start, end)
                if start < shift.starts_at or end > shift.ends_at:
                    issues.append(
                        ParseIssue(
                            "uni_outside_shift",
                            f"SPS interval {token.strip()!r} is outside the source shift",
                            source_id,
                        )
                    )
                    continue
                intervals.append(
                    SpsInterval(
                        key=f"{source_id}:{ordinal}",
                        source_id=source_id,
                        ordinal=ordinal,
                        interval=interval,
                    )
                )

    intervals.sort(key=lambda item: item.interval.starts_at)
    deduplicated: list[SpsInterval] = []
    for current in intervals:
        if deduplicated and current.interval == deduplicated[-1].interval:
            issues.append(
                ParseIssue(
                    "duplicate_uni_interval",
                    "The same SPS interval appears more than once; it will be planned once",
                    current.source_id,
                )
            )
            continue
        if (
            deduplicated
            and current.interval.starts_at < deduplicated[-1].interval.ends_at
        ):
            issues.append(
                ParseIssue(
                    "overlapping_uni_intervals",
                    "SPS intervals overlap and require review",
                    current.source_id,
                )
            )
            continue
        deduplicated.append(current)

    return SpsParseResult(
        tuple(deduplicated), tuple(issues), tuple(dict.fromkeys(relevant))
    )


def _localize(
    day: date, clock: time, timezone: ZoneInfo
) -> tuple[datetime | None, str | None]:
    naive = datetime.combine(day, clock)
    candidates: dict[datetime, datetime] = {}
    for fold in (0, 1):
        aware = naive.replace(tzinfo=timezone, fold=fold)
        round_trip = aware.astimezone(UTC).astimezone(timezone).replace(tzinfo=None)
        if round_trip == naive:
            candidates[aware.astimezone(UTC)] = aware
    if not candidates:
        return None, "nonexistent"
    if len(candidates) > 1:
        return None, "ambiguous"
    return next(iter(candidates.values())), None
