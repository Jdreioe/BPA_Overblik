"""Danish, display-ready view of one week's plan.

The GUI must never parse the planner's English summaries, step keys or
hashes. This module turns a :class:`~.models.SyncPlan` into structured,
already-translated fields: a Monday-to-Sunday grid of MitHF shift blocks,
the items that need attention first, and the exact approval summary shown
beside "Overfør ændringer".

Every decision here is presentation. Reconciliation, blocking and approval
digests stay in ``planner``/``sync``; this module only reads their result.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field, replace
from datetime import date, datetime, time, timedelta
from typing import Any
from zoneinfo import ZoneInfo

from .models import (
    AppConfig,
    DestinationSnapshot,
    Outcome,
    PlanItem,
    SourceShift,
    SyncPlan,
    step_base,
    step_segment,
)
from .sync import BLOCKERS, WRITES

WEEKDAYS = ("man", "tir", "ons", "tor", "fre", "lør", "søn")
MONTHS = (
    "jan",
    "feb",
    "mar",
    "apr",
    "maj",
    "jun",
    "jul",
    "aug",
    "sep",
    "okt",
    "nov",
    "dec",
)

#: Plain-language cause and next action for every blocking reason the
#: planner can report. Keyed by ``PlanItem.reason``.
EXPLANATIONS: dict[str, tuple[str, str]] = {
    # Source instructions in TeamUp
    "unsupported_uni_syntax": (
        "En uni-linje i TeamUp kan ikke læses.",
        "Ret linjen i TeamUp, for eksempel »uni 8-10 & 13-14«.",
    ),
    "invalid_uni_interval": (
        "Et SPS-tidsrum kan ikke læses.",
        "Skriv tidsrummet som 8-10 eller 08:00-10:00.",
    ),
    "invalid_uni_date": (
        "Datoen i uni-linjen kan ikke læses.",
        "Skriv datoen som 2026-09-15.",
    ),
    "ambiguous_uni_date": (
        "Vagten dækker mere end ét døgn, og uni-linjen siger ikke hvilken dag.",
        "Tilføj dag eller dato, for eksempel »uni 23-1 fredag«.",
    ),
    "ambiguous_uni_weekday": (
        "Vagten indeholder den nævnte ugedag mere end én gang.",
        "Skriv en dato i stedet for ugedagen.",
    ),
    "conflicting_uni_date": (
        "Datoen og ugedagen i uni-linjen passer ikke sammen.",
        "Ret den ene af dem i TeamUp.",
    ),
    "uni_weekday_outside_shift": (
        "Den nævnte ugedag ligger uden for vagten.",
        "Ret ugedagen eller vagtens tidsrum i TeamUp.",
    ),
    "uni_outside_shift": (
        "Et SPS-tidsrum ligger uden for vagten.",
        "Ret tidsrummet eller vagtens tidsrum i TeamUp.",
    ),
    "overlapping_uni_intervals": (
        "To SPS-tidsrum på vagten overlapper hinanden.",
        "Ret tidsrummene i TeamUp, så de ikke dækker de samme timer.",
    ),
    "nonexistent_local_time": (
        "Et SPS-tidsrum rammer en time, der ikke findes på grund af sommertid.",
        "Skriv et tidsrum uden for tidsomstillingen.",
    ),
    "ambiguous_local_time": (
        "Et SPS-tidsrum rammer en time, der findes to gange på grund af sommertid.",
        "Skriv et entydigt tidsrum i TeamUp.",
    ),
    # Setup
    "no_helper_mapping": (
        "Hjælperen fra TeamUp er ikke koblet til MitHF og DUOS.",
        "Vælg hjælperen under Bekræft hjælpere i opsætningen.",
    ),
    "missing_configuration": (
        "DUOS-ordning og registreringstype er ikke bekræftet.",
        "Vælg ordning og type under Bekræft ordning i opsætningen.",
    ),
    # Destination state
    "destination_missing": (
        "En tidligere overført post findes ikke længere i destinationen.",
        (
            "Er den slettet med vilje, så lad den være. Skal den laves igen, "
            "så tillad overførsel igen for vagten."
        ),
    ),
    "manually_changed": (
        "Posten er rettet i hånden i destinationen efter sidste overførsel.",
        "Ret den i destinationen eller i TeamUp, så de er enige.",
    ),
    "not_unique": (
        "Flere poster i destinationen passer på den samme vagt.",
        "Fjern dubletten i destinationen, og hent ugen igen.",
    ),
    "overlapping": (
        "Der ligger allerede timer i destinationen oven i dette tidsrum.",
        "Ret den eksisterende post i destinationen, og hent ugen igen.",
    ),
    "assigned_to_other": (
        "Vagten i MitHF er tildelt en anden hjælper.",
        "Ret hjælperen i MitHF eller i TeamUp.",
    ),
    "existing_differs": (
        "Destinationen har allerede andre timer på vagten end TeamUp.",
        "Kontrollér hvilke timer der er rigtige, og ret dem ét sted.",
    ),
    "changed_since_sync": (
        "SPS-timerne er ændret i MitHF efter sidste overførsel.",
        "Kontrollér timerne i MitHF, og ret dem ét sted.",
    ),
    "removed_from_source": (
        "Noget, der tidligere er overført, findes ikke længere i TeamUp.",
        (
            "Appen sletter aldrig af sig selv. Fjern det i destinationen, "
            "hvis det skal væk."
        ),
    ),
    "extra_segment_removed": (
        "En vagt blev delt op for flere SPS-tidsrum, og et af dem er væk igen.",
        "Slet den overflødige vagt i MitHF, hvis den ikke skal bruges.",
    ),
    "uncertain_write": (
        "En tidligere overførsel nåede ikke at blive bekræftet.",
        "Kontrollér i destinationen, om den blev gemt, før du overfører igen.",
    ),
    "not_pending": (
        "DUOS-registreringen afventer ikke længere godkendelse.",
        "Ret den i DUOS, hvis timerne skal ændres.",
    ),
    "rejected": (
        "DUOS-registreringen er afvist, trukket tilbage eller modregnet.",
        "Kontrollér registreringen i DUOS.",
    ),
    "blocked_by_shift": (
        "Trinnet venter på, at vagten selv kommer på plads.",
        "Løs problemet på vagten ovenfor først.",
    ),
    "source_issue": (
        "Trinnet venter på, at uni-linjen på vagten bliver rettet.",
        "Ret uni-linjen i TeamUp, og hent ugen igen.",
    ),
    "offline_preview": (
        "MitHF og DUOS er ikke aflæst i denne visning.",
        "Log ind i begge tjenester for at se de rigtige ændringer.",
    ),
}

_FALLBACK = (
    "Punktet kunne ikke afgøres sikkert.",
    "Kontrollér vagten i TeamUp og i destinationen, og hent ugen igen.",
)

STATUS_LABELS = {
    "create": "Oprettes",
    "update": "Ændres",
    "matched": "Uændret",
    "pending": "Ikke aflæst",
    "attention": "Kræver opmærksomhed",
}


@dataclass(frozen=True)
class Block:
    """One MitHF shift as drawn in a single day column of the week grid."""

    helper: str
    #: The helper's own Teamup calendar colour as hex, so the week reads the
    #: way it does in Teamup. Empty when unknown; the block renders neutral.
    helper_color: str
    status: str
    status_label: str
    #: Minutes from local midnight; the grid draws the block between them.
    minutes_from: int
    minutes_to: int
    time_label: str
    sps_label: str
    part_label: str
    continues_before: bool
    continues_after: bool
    details: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class Day:
    date: str
    label: str
    blocks: list[Block] = field(default_factory=list)


@dataclass(frozen=True)
class Attention:
    when: str
    who: str
    explanation: str
    action: str


@dataclass(frozen=True)
class WeekPreview:
    days: list[Day]
    attention: list[Attention]
    headline: str
    notice: str
    summary: list[str]
    apply_summary: str
    can_apply: bool
    blocked_reason: str
    destination_read: bool


def build_week_preview(
    *,
    config: AppConfig,
    shifts: tuple[SourceShift, ...],
    plan: SyncPlan,
    destination: DestinationSnapshot,
    destination_read: bool,
) -> WeekPreview:
    timezone = ZoneInfo(config.timezone)
    by_source: dict[str, list[PlanItem]] = {}
    for item in plan.items:
        by_source.setdefault(item.source_key, []).append(item)

    start = plan.starts_at.astimezone(timezone).date()
    end = (plan.ends_at - timedelta(microseconds=1)).astimezone(timezone).date()
    days = {
        start + timedelta(days=offset): [] for offset in range((end - start).days + 1)
    }
    attention: list[Attention] = []
    seen: set[tuple[str, str, str, str]] = set()
    planned_shifts = 0

    for shift in sorted(shifts, key=lambda value: value.starts_at):
        items = by_source.get(shift.key)
        if not items:
            continue
        if all(item.outcome == Outcome.EXCLUDED for item in items):
            continue
        planned_shifts += 1
        helper = _helper_name(config, shift)
        helper_color = config.teamup_subcalendar_colors.get(shift.helper_key, "")
        segments = _segments(items)
        # A source or mapping problem belongs to the whole shift, so every
        # block it produced must be marked, not only the step that failed.
        shift_blocked = any(
            item.system in {"source", "mapping"} and item.outcome in BLOCKERS
            for item in items
        )
        for index, segment in sorted(segments.items()):
            described = _describe_segment(
                helper,
                helper_color,
                segment,
                destination,
                timezone,
                index,
                len(segments),
                shift_blocked,
            )
            if described is None:
                continue
            for day, piece in _split_by_day(*described):
                if day in days:
                    days[day].append(piece)
        for item in items:
            # An unread destination blocks the whole week, not one shift; it
            # is stated once in the notice rather than repeated per shift.
            if item.outcome not in BLOCKERS or item.reason == "offline_preview":
                continue
            explanation, action = EXPLANATIONS.get(item.reason, _FALLBACK)
            entry = (_moment(shift.starts_at, timezone), helper, explanation, action)
            if entry in seen:
                continue
            seen.add(entry)
            attention.append(Attention(*entry))

    counts = _counts(plan)
    summary = _summary_lines(counts)
    has_writes = counts["created"] + counts["updated"] + counts["duos"] > 0
    can_apply = destination_read and not attention and has_writes
    # A blocked source item can be the only thing standing in the way even
    # when no destination step reported it, so gate on the plan as well.
    can_apply = can_apply and not any(item.outcome in BLOCKERS for item in plan.items)
    return WeekPreview(
        days=[
            Day(
                day.isoformat(),
                f"{WEEKDAYS[day.weekday()]} {day.day}. {MONTHS[day.month - 1]}",
                sorted(blocks, key=lambda block: block.minutes_from),
            )
            for day, blocks in sorted(days.items())
        ],
        attention=attention,
        headline=_headline(planned_shifts, attention, has_writes, destination_read),
        notice=_notice(counts, destination_read),
        summary=summary,
        apply_summary=_apply_summary(counts),
        can_apply=can_apply,
        blocked_reason=_blocked_reason(
            attention, has_writes, destination_read, planned_shifts
        ),
        destination_read=destination_read,
    )


def to_dict(preview: WeekPreview) -> dict[str, Any]:
    return asdict(preview)


def _helper_name(config: AppConfig, shift: SourceShift) -> str:
    mapping = config.helpers.get(shift.helper_key)
    if mapping:
        return mapping.mithf_name
    return config.teamup_subcalendar_helpers.get(shift.helper_key, "Ukendt hjælper")


def _segments(items: list[PlanItem]) -> dict[int, dict[str, list[PlanItem]]]:
    """Group a shift's items by the MitHF shift segment they belong to.

    DUOS steps have no segment of their own; they are attached to the segment
    whose hours contain their interval once the MitHF bounds are known.
    """
    segments: dict[int, dict[str, list[PlanItem]]] = {}
    duos: list[PlanItem] = []
    for item in items:
        if item.system == "duos":
            duos.append(item)
            continue
        if item.system != "mithf":
            continue
        index = step_segment(item.step_key)
        segments.setdefault(index, {}).setdefault(step_base(item.step_key), []).append(
            item
        )
    if not segments:
        return {}
    for item in duos:
        index = _segment_for(item, segments)
        segments[index].setdefault("duos", []).append(item)
    return segments


def _segment_for(item: PlanItem, segments: dict[int, dict[str, list[PlanItem]]]) -> int:
    starts_at = item.payload.get("starts_at")
    for index in sorted(segments):
        create = segments[index].get("mithf.create_shift")
        if not create or not starts_at:
            continue
        payload = create[0].payload
        if payload["starts_at"] <= starts_at < payload["ends_at"]:
            return index
    return min(segments)


def _describe_segment(
    helper: str,
    helper_color: str,
    segment: dict[str, list[PlanItem]],
    destination: DestinationSnapshot,
    timezone: ZoneInfo,
    index: int,
    total: int,
    shift_blocked: bool,
) -> tuple[datetime, datetime, Block] | None:
    create = segment.get("mithf.create_shift")
    if not create:
        return None
    payload = create[0].payload
    if "starts_at" not in payload:
        return None
    starts_at = datetime.fromisoformat(payload["starts_at"]).astimezone(timezone)
    ends_at = datetime.fromisoformat(payload["ends_at"]).astimezone(timezone)

    details: list[str] = []
    sps_label = ""
    flat = [item for group in segment.values() for item in group]
    for item in flat:
        line = _detail_line(item, destination, timezone)
        if line:
            details.append(line)
        if step_base(item.step_key) == "mithf.set_sps" and item.payload.get(
            "intervals"
        ):
            sps_label = ", ".join(
                _interval_label(entry, timezone) for entry in item.payload["intervals"]
            )

    status = "attention" if shift_blocked else _status(flat, create[0])
    block = Block(
        helper=helper,
        helper_color=helper_color,
        status=status,
        status_label=STATUS_LABELS[status],
        minutes_from=0,
        minutes_to=1440,
        time_label=_span_label(starts_at, ends_at),
        sps_label=sps_label,
        part_label=f"Del {index + 1} af {total}" if total > 1 else "",
        continues_before=False,
        continues_after=False,
        details=details,
    )
    return starts_at, ends_at, block


def _status(items: list[PlanItem], create: PlanItem) -> str:
    if any(item.outcome in BLOCKERS - {Outcome.PENDING_INTEGRATION} for item in items):
        return "attention"
    if create.outcome == Outcome.WOULD_CREATE:
        return "create"
    if any(item.outcome in WRITES for item in items):
        return "update"
    if any(item.outcome == Outcome.PENDING_INTEGRATION for item in items):
        return "pending"
    return "matched"


def _detail_line(
    item: PlanItem, destination: DestinationSnapshot, timezone: ZoneInfo
) -> str:
    base = step_base(item.step_key)
    if item.outcome in BLOCKERS - {Outcome.PENDING_INTEGRATION}:
        return ""
    if base == "mithf.create_shift":
        if item.outcome == Outcome.WOULD_CREATE:
            return "Vagten oprettes i MitHF."
        if item.outcome == Outcome.WOULD_UPDATE:
            before = _existing_shift_label(item, destination, timezone)
            after = _span_label(
                datetime.fromisoformat(item.payload["starts_at"]).astimezone(timezone),
                datetime.fromisoformat(item.payload["ends_at"]).astimezone(timezone),
            )
            return f"Vagtens tid ændres i MitHF: {before} → {after}."
        return ""
    if base == "mithf.assign_helper":
        name = item.payload.get("helper_name", "")
        if item.outcome == Outcome.WOULD_CREATE:
            return f"{name} sættes på vagten."
        if item.outcome == Outcome.WOULD_UPDATE:
            return f"Vagten tildeles {name}."
        return ""
    if base == "mithf.set_sps":
        intervals = item.payload.get("intervals") or []
        after = ", ".join(_interval_label(entry, timezone) for entry in intervals)
        if item.outcome == Outcome.WOULD_CREATE:
            return f"SPS-timer sættes til {after}."
        if item.outcome == Outcome.WOULD_UPDATE:
            before = _existing_sps_label(item, destination, timezone)
            return f"SPS-timer ændres: {before} → {after}."
        if item.outcome == Outcome.PENDING_INTEGRATION:
            return f"SPS-timer {after} kræver aflæsning af MitHF."
        if item.outcome == Outcome.ALREADY_MATCHED:
            return f"SPS-timer {after} er allerede sat."
        return ""
    if base == "mithf.set_meeting":
        if item.outcome in WRITES:
            return "Vagtmøde sættes på hele vagten."
        if item.outcome == Outcome.PENDING_INTEGRATION:
            return "Vagtmøde kræver aflæsning af MitHF."
        return ""
    if item.system == "duos":
        label = _interval_label(item.payload, timezone)
        hours = _hours_label(item.payload)
        if item.outcome == Outcome.WOULD_CREATE:
            return f"DUOS: {hours} registreres for {label}."
        if item.outcome == Outcome.WOULD_UPDATE:
            return f"DUOS: registreringen rettes til {label} ({hours})."
        if item.outcome == Outcome.EXCLUDED and item.reason == "future_hours":
            return f"DUOS: {label} kan overføres, når timerne er afsluttet."
        if item.outcome == Outcome.ALREADY_MATCHED:
            return f"DUOS: {hours} er allerede registreret."
    return ""


def _existing_shift_label(
    item: PlanItem, destination: DestinationSnapshot, timezone: ZoneInfo
) -> str:
    existing = next(
        (s for s in destination.mithf_shifts if s.id == item.destination_id), None
    )
    if existing is None:
        return "ukendt tid"
    return _span_label(
        existing.starts_at.astimezone(timezone), existing.ends_at.astimezone(timezone)
    )


def _existing_sps_label(
    item: PlanItem, destination: DestinationSnapshot, timezone: ZoneInfo
) -> str:
    existing = next(
        (s for s in destination.mithf_shifts if s.id == item.destination_id), None
    )
    if existing is None or not existing.sps_intervals:
        return "ingen SPS-timer"
    return ", ".join(
        _span_label(
            interval.starts_at.astimezone(timezone),
            interval.ends_at.astimezone(timezone),
        )
        for interval in existing.sps_intervals
    )


def _interval_label(payload: dict[str, Any], timezone: ZoneInfo) -> str:
    return _span_label(
        datetime.fromisoformat(payload["starts_at"]).astimezone(timezone),
        datetime.fromisoformat(payload["ends_at"]).astimezone(timezone),
    )


def _hours_label(payload: dict[str, Any]) -> str:
    start = datetime.fromisoformat(payload["starts_at"])
    end = datetime.fromisoformat(payload["ends_at"])
    hours = (end.timestamp() - start.timestamp()) / 3600
    text = f"{hours:g}".replace(".", ",")
    return f"{text} time" if hours == 1 else f"{text} timer"


def _span_label(start: datetime, end: datetime) -> str:
    """Show both dates whenever a span crosses local midnight.

    A span ending exactly at midnight reads as 24:00 on its own day, the way
    MitHF writes it, rather than as 00:00 on the next one.
    """
    if _minutes(end) == 0 and end.date() == start.date() + timedelta(days=1):
        return f"{start:%H:%M}–24:00"
    if start.date() == end.date():
        return f"{start:%H:%M}–{end:%H:%M}"
    return (
        f"{start:%H:%M} {start.day}. {MONTHS[start.month - 1]}"
        f" – {end:%H:%M} {end.day}. {MONTHS[end.month - 1]}"
    )


def _moment(value: datetime, timezone: ZoneInfo) -> str:
    local = value.astimezone(timezone)
    return (
        f"{WEEKDAYS[local.weekday()]} {local.day}. {MONTHS[local.month - 1]}"
        f" {local:%H:%M}"
    )


def _minutes(value: datetime) -> int:
    return value.hour * 60 + value.minute


def _split_by_day(
    starts_at: datetime, ends_at: datetime, block: Block
) -> list[tuple[date, Block]]:
    """Cut a shift at local midnight so every day column draws its own hours.

    An overnight shift therefore appears in both days, marked as continuing,
    while its label keeps both real dates.
    """
    pieces: list[tuple[date, Block]] = []
    day = starts_at.date()
    cursor = starts_at
    while cursor < ends_at:
        midnight = datetime.combine(day + timedelta(days=1), time.min, cursor.tzinfo)
        piece_end = min(ends_at, midnight)
        pieces.append(
            (
                day,
                replace(
                    block,
                    minutes_from=_minutes(cursor),
                    minutes_to=1440 if piece_end == midnight else _minutes(piece_end),
                    continues_before=cursor != starts_at,
                    continues_after=piece_end != ends_at,
                ),
            )
        )
        cursor = piece_end
        day += timedelta(days=1)
    return pieces


def _counts(plan: SyncPlan) -> dict[str, int]:
    counts = dict.fromkeys(
        ("created", "updated", "matched", "sps", "duos", "future", "attention"), 0
    )
    for item in plan.items:
        base = step_base(item.step_key)
        if base == "mithf.create_shift":
            if item.outcome == Outcome.WOULD_CREATE:
                counts["created"] += 1
            elif item.outcome == Outcome.WOULD_UPDATE:
                counts["updated"] += 1
            elif item.outcome == Outcome.ALREADY_MATCHED:
                counts["matched"] += 1
        elif base == "mithf.set_sps" and item.outcome in WRITES:
            counts["sps"] += 1
        elif item.system == "duos":
            if item.outcome in WRITES:
                counts["duos"] += 1
            elif item.outcome == Outcome.EXCLUDED and item.reason == "future_hours":
                counts["future"] += 1
        if item.outcome in BLOCKERS:
            counts["attention"] += 1
    return counts


def _plural(count: int, singular: str, plural: str) -> str:
    return f"{count} {singular if count == 1 else plural}"


def _summary_lines(counts: dict[str, int]) -> list[str]:
    lines: list[str] = []
    if counts["created"]:
        lines.append(_plural(counts["created"], "ny vagt", "nye vagter") + " i MitHF")
    if counts["updated"]:
        lines.append(
            _plural(counts["updated"], "ændret vagt", "ændrede vagter") + " i MitHF"
        )
    if counts["sps"]:
        lines.append(_plural(counts["sps"], "SPS-tidsrum", "SPS-tidsrum") + " i MitHF")
    if counts["duos"]:
        lines.append(
            _plural(counts["duos"], "registrering", "registreringer") + " i DUOS"
        )
    if counts["matched"]:
        lines.append(
            _plural(counts["matched"], "vagt", "vagter") + " er allerede på plads"
        )
    if counts["future"]:
        lines.append(
            _plural(counts["future"], "SPS-tidsrum", "SPS-tidsrum")
            + " kan overføres, når timerne er afsluttet"
        )
    return lines


def _apply_summary(counts: dict[str, int]) -> str:
    parts: list[str] = []
    if counts["created"]:
        parts.append(_plural(counts["created"], "ny vagt", "nye vagter"))
    if counts["updated"]:
        parts.append(_plural(counts["updated"], "ændret vagt", "ændrede vagter"))
    if counts["sps"]:
        parts.append(_plural(counts["sps"], "SPS-tidsrum", "SPS-tidsrum"))
    duos = (
        _plural(counts["duos"], "registrering", "registreringer")
        if counts["duos"]
        else ""
    )
    mithf = ", ".join(parts)
    if mithf and duos:
        return f"Overfører {mithf} til MitHF og {duos} til DUOS."
    if mithf:
        return f"Overfører {mithf} til MitHF."
    if duos:
        return f"Overfører {duos} til DUOS."
    return "Der er ingen ændringer at overføre."


def _headline(
    planned_shifts: int,
    attention: list[Attention],
    has_writes: bool,
    destination_read: bool,
) -> str:
    if not planned_shifts:
        return "Der er ingen vagter i denne uge."
    if attention:
        return (
            _plural(len(attention), "punkt kræver", "punkter kræver")
            + " opmærksomhed, før ugen kan overføres."
        )
    if not destination_read:
        return "MitHF og DUOS er ikke aflæst, så ugen kan ikke overføres endnu."
    if not has_writes:
        return "Ugen er allerede overført. Der er ingen ændringer."
    return "Ugen er klar til overførsel."


def _notice(counts: dict[str, int], destination_read: bool) -> str:
    if not destination_read:
        return (
            "MitHF og DUOS er ikke aflæst. Visningen viser, hvad TeamUp beder om, "
            "ikke hvad der allerede findes i tjenesterne."
        )
    if counts["future"]:
        return "DUOS-timer registreres først, når tidsrummet er afsluttet."
    return ""


def _blocked_reason(
    attention: list[Attention],
    has_writes: bool,
    destination_read: bool,
    planned_shifts: int,
) -> str:
    if not destination_read:
        return "Log ind i MitHF og DUOS, så ugen kan aflæses."
    if attention:
        return "Løs punkterne under Kræver opmærksomhed først."
    if not planned_shifts:
        return "Der er ingen vagter at overføre."
    if not has_writes:
        return "Der er ingen ændringer at overføre."
    return ""
