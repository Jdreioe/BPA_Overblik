from __future__ import annotations

import argparse
import shutil
import sys
from datetime import date, datetime, time, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from .browser import BrowserTransport, DestinationError
from .config import ConfigError, configuration_issues, load_config
from .destinations import Destinations
from .environment import load_local_environment
from .fixtures import FixtureError, load_fixture
from .models import AppConfig, Outcome, PlanItem, SyncPlan
from .parser import parse_sps_instructions
from .planner import build_plan
from .report import render_plan
from .source_rules import is_reminder, meeting_item
from .state import SyncState
from .sync import BLOCKERS, apply_plan, plan_digest, reconciliation_range
from .teamup import TeamUpClient, TeamUpError


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if hasattr(args, "config"):
        load_local_environment(args.config)
    try:
        if args.command == "init":
            return _init(args)
        if args.command in {"dry-run", "apply"}:
            return _dry_run(args)
        if args.command == "forget":
            return _forget(args)
        if args.command == "teamup-probe":
            return _teamup_probe(args)
        if args.command == "check-config":
            config = load_config(args.config)
            ZoneInfo(config.timezone)
            issues = configuration_issues(config)
            for issue in issues:
                print(f"Missing or inconsistent: {issue}")
            if not issues:
                print(
                    "Local configuration is complete; live identities remain to be verified."
                )
            print("Use dry-run --live --cdp-url to verify authenticated destinations.")
            return 1 if issues else 0
    except (
        ConfigError,
        FixtureError,
        TeamUpError,
        DestinationError,
        ValueError,
        ZoneInfoNotFoundError,
    ) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    parser.error("a command is required")
    return 2


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="teamup-shift-sync",
        description="Plan a manual TeamUp to MitHF and DUOS weekly synchronization.",
    )
    commands = parser.add_subparsers(dest="command")

    init = commands.add_parser("init", help="Create local config and SQLite state")
    init.add_argument("--config", type=Path, default=Path("config.toml"))
    init.add_argument("--example", type=Path, default=Path("config.example.toml"))
    init.add_argument("--state", type=Path, default=Path(".local/sync.sqlite3"))

    dry_run = commands.add_parser(
        "dry-run", help="Build a read-only synchronization preview"
    )
    dry_run.add_argument("--config", type=Path, default=Path("config.toml"))
    source = dry_run.add_mutually_exclusive_group(required=True)
    source.add_argument("--fixture", type=Path)
    source.add_argument(
        "--live",
        action="store_true",
        help="Read TeamUp; destination reconciliation remains pending",
    )
    dry_run.add_argument("--state", type=Path, default=Path(".local/sync.sqlite3"))
    dry_run.add_argument("--from", dest="from_date", type=_date)
    dry_run.add_argument("--to", dest="to_date", type=_date)
    dry_run.add_argument("--now", type=_aware_datetime)
    dry_run.add_argument(
        "--cdp-url", help="Chromium CDP endpoint with authenticated MitHF and DUOS tabs"
    )

    apply = commands.add_parser(
        "apply", help="Apply a reviewed live plan and verify every step"
    )
    apply.set_defaults(live=True, now=None)
    apply.add_argument("--config", type=Path, default=Path("config.toml"))
    apply.add_argument("--state", type=Path, default=Path(".local/sync.sqlite3"))
    apply.add_argument("--from", dest="from_date", type=_date, required=True)
    apply.add_argument("--to", dest="to_date", type=_date, required=True)
    apply.add_argument("--cdp-url", required=True)
    apply.add_argument(
        "--approve",
        required=True,
        help="Exact plan digest from the reviewed live dry-run",
    )

    forget = commands.add_parser(
        "forget",
        help="Drop stored sync records for a shift so TeamUp is synchronized again",
    )
    forget.add_argument("--state", type=Path, default=Path(".local/sync.sqlite3"))
    forget.add_argument(
        "--shift",
        nargs="+",
        required=True,
        metavar="SOURCE_KEY",
        help="Source keys as printed in the plan report (calendar:event:occurrence)",
    )

    probe = commands.add_parser(
        "teamup-probe", help="Read a TeamUp week and validate event/comment access"
    )
    probe.add_argument("--config", type=Path, default=Path("config.toml"))
    probe.add_argument("--from", dest="from_date", type=_date)
    probe.add_argument("--to", dest="to_date", type=_date)
    check = commands.add_parser(
        "check-config", help="Check local sync configuration without service requests"
    )
    check.add_argument("--config", type=Path, default=Path("config.toml"))
    return parser


def _init(args: argparse.Namespace) -> int:
    if args.config.exists():
        raise ValueError(f"Refusing to overwrite existing configuration: {args.config}")
    if not args.example.exists():
        raise ValueError(f"Configuration example not found: {args.example}")
    args.config.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.example, args.config)
    with SyncState(args.state):
        pass
    print(f"Created {args.config} and initialized {args.state}")
    print(
        "Fill in validated identities and credentials before using live integrations."
    )
    return 0


def _dry_run(args: argparse.Namespace) -> int:
    config = load_config(args.config)
    timezone = ZoneInfo(config.timezone)
    now = args.now or datetime.now(timezone)
    if now.tzinfo is None:
        raise ValueError("--now must include a UTC offset")
    default_monday = now.astimezone(timezone).date() - timedelta(
        days=now.astimezone(timezone).weekday()
    )
    from_date = args.from_date or default_monday
    to_date = args.to_date or (from_date + timedelta(days=6))
    if to_date < from_date:
        raise ValueError("--to must be on or after --from")
    range_start = datetime.combine(from_date, time.min, timezone)
    range_end = datetime.combine(to_date + timedelta(days=1), time.min, timezone)
    if args.live:
        if args.cdp_url:
            return _destination_plan(
                args, config, from_date, to_date, range_start, range_end, now
            )
        plan = _live_source_plan(
            config, from_date, to_date, range_start, range_end, now
        )
        print(render_plan(plan))
        return 1 if any(item.outcome == Outcome.REVIEW for item in plan.items) else 0
    shifts, destination = load_fixture(args.fixture)
    with SyncState(args.state) as state:
        plan = build_plan(
            config=config,
            shifts=shifts,
            destination=destination,
            range_start=range_start,
            range_end=range_end,
            now=now,
            state=state,
        )
    print(render_plan(plan))
    return (
        1
        if any(item.outcome.value in {"conflicted", "failed"} for item in plan.items)
        else 0
    )


def _destination_plan(args, config, from_date, to_date, range_start, range_end, now):
    issues = configuration_issues(config)
    if issues:
        raise ConfigError("; ".join(issues))
    client = TeamUpClient(config)
    shifts = tuple(
        client.to_source_shift(o, config.teamup_helper_field)
        for o in client.fetch_occurrences(from_date, to_date)
        if not is_reminder(o.title)
    )
    with BrowserTransport(args.cdp_url) as transport, SyncState(args.state) as state:
        destinations = Destinations(transport, config)
        destinations.validate(range_start)
        if args.command == "apply":
            apply_plan(
                config=config,
                shifts=shifts,
                destinations=destinations,
                state=state,
                start=range_start,
                end=range_end,
                expected_digest=args.approve,
                now=now,
            )
            print(
                "Synchronization finished; all selected operations were read back and verified."
            )
            return 0
        plan = build_plan(
            config=config,
            shifts=shifts,
            destination=destinations.read(*reconciliation_range(shifts, range_start, range_end)),
            range_start=range_start,
            range_end=range_end,
            now=now,
            state=state,
            live=True,
        )
    print(render_plan(plan))
    print(f"Plan digest: {plan_digest(plan)}")
    return 1 if any(i.outcome in BLOCKERS for i in plan.items) else 0


def _live_source_plan(
    config: AppConfig,
    from_date: date,
    to_date: date,
    range_start: datetime,
    range_end: datetime,
    now: datetime,
) -> SyncPlan:
    client = TeamUpClient(config)
    items: list[PlanItem] = []
    for occurrence in client.fetch_occurrences(from_date, to_date):
        if occurrence.ends_at <= range_start or occurrence.starts_at >= range_end:
            continue
        key = f"{client.calendar_id}:{occurrence.series_id}:{occurrence.id}"
        if is_reminder(occurrence.title):
            items.append(
                PlanItem(
                    key,
                    "source",
                    "source.reminder",
                    Outcome.EXCLUDED,
                    "Shared 'Husk at checke' reminder, not a shift",
                )
            )
            continue
        meeting = meeting_item(
            occurrence.title, key, occurrence.starts_at, occurrence.ends_at
        )
        if meeting:
            items.append(meeting)
        try:
            shift = client.to_source_shift(occurrence, config.teamup_helper_field)
        except TeamUpError as error:
            items.append(
                PlanItem(key, "source", "source.helper", Outcome.REVIEW, str(error))
            )
            continue
        parsed = parse_sps_instructions(shift, ZoneInfo(config.timezone))
        helper = config.teamup_subcalendar_helpers.get(
            shift.helper_key, shift.helper_key
        )
        items.append(
            PlanItem(
                key,
                "mithf",
                "mithf.readback",
                Outcome.PENDING_INTEGRATION,
                f"{helper}: {shift.starts_at.isoformat()} to {shift.ends_at.isoformat()}; destination not reconciled",
            )
        )
        for issue in parsed.issues:
            items.append(
                PlanItem(
                    key,
                    "source",
                    f"sps:{issue.source_id}:{issue.code}",
                    Outcome.REVIEW,
                    issue.message,
                )
            )
        for interval in parsed.intervals:
            future = interval.interval.ends_at > now
            outcome = (
                Outcome.EXCLUDED
                if future
                else (Outcome.REVIEW if parsed.issues else Outcome.PENDING_INTEGRATION)
            )
            items.append(
                PlanItem(
                    key,
                    "duos",
                    interval.key,
                    outcome,
                    f"{interval.interval.starts_at.isoformat()} to {interval.interval.ends_at.isoformat()} ({interval.interval.hours:g} hours): "
                    + (
                        "future or ongoing"
                        if future
                        else "destination reconciliation required"
                    ),
                    {
                        "starts_at": interval.interval.starts_at.isoformat(),
                        "ends_at": interval.interval.ends_at.isoformat(),
                        "hours": interval.interval.hours,
                    },
                )
            )
        if len(parsed.intervals) > 1:
            items.append(
                PlanItem(
                    key,
                    "mithf",
                    "mithf.sps",
                    Outcome.PENDING_MITHF_BUG,
                    "Separate SPS intervals preserved; no merging or workaround",
                )
            )
    return SyncPlan(range_start, range_end, now, tuple(items))


def _forget(args: argparse.Namespace) -> int:
    """Forget stored steps so the next plan rebuilds them from TeamUp.

    This only clears local memory of what was synchronized. It never writes to
    MitHF or DUOS, and the next dry-run still has to be reviewed and approved.
    """
    with SyncState(args.state) as state, state.exclusive_apply():
        forgotten = {key: state.forget_steps(key) for key in args.shift}
    for key, steps in forgotten.items():
        print(f"{key}: {', '.join(steps) if steps else 'no stored records'}")
    if not any(forgotten.values()):
        sys.stdout.flush()
        print("No stored records matched; nothing was forgotten.", file=sys.stderr)
        return 1
    print(
        "Run dry-run again to review the rebuilt plan. Destination records that "
        "still exist are matched by value, not created a second time."
    )
    return 0


def _teamup_probe(args: argparse.Namespace) -> int:
    config = load_config(args.config)
    timezone = ZoneInfo(config.timezone)
    today = datetime.now(timezone).date()
    monday = today - timedelta(days=today.weekday())
    from_date = args.from_date or monday
    to_date = args.to_date or (from_date + timedelta(days=6))
    if to_date < from_date:
        raise ValueError("--to must be on or after --from")
    client = TeamUpClient(config)
    occurrences = client.fetch_occurrences(from_date, to_date)
    recurring = sum(item.id != item.series_id for item in occurrences)
    comments = sum(len(item.comments) for item in occurrences)
    print("TeamUp read-only probe succeeded.")
    print(f"Range: {from_date.isoformat()} to {to_date.isoformat()} (inclusive)")
    print(
        f"Events: {len(occurrences)}; recurring occurrences: {recurring}; comments: {comments}"
    )
    print("No event titles, helper names, or comment text were printed.")
    if config.teamup_helper_field:
        resolved = 0
        excluded = 0
        for occurrence in occurrences:
            if is_reminder(occurrence.title):
                excluded += 1
                continue
            try:
                client.to_source_shift(occurrence, config.teamup_helper_field)
            except TeamUpError:
                continue
            resolved += 1
        print(
            f"Source shifts resolved: {resolved}; excluded reminders: {excluded}; requiring review: {len(occurrences) - resolved - excluded}"
        )
        if resolved + excluded != len(occurrences):
            return 1
    return 0


def _date(value: str) -> date:
    try:
        return date.fromisoformat(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected YYYY-MM-DD") from error


def _aware_datetime(value: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected an ISO-8601 datetime") from error
    if parsed.tzinfo is None:
        raise argparse.ArgumentTypeError("datetime must include a UTC offset")
    return parsed
