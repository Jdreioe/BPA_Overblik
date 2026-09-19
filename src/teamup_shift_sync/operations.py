"""Shared application operations for the CLI and the desktop GUI worker.

The CLI historically mixed argument parsing, network orchestration, and
human-readable reporting in ``cli.py``. The GUI must not parse CLI reports,
so this module exposes the same orchestration as callable operations that
return typed results. ``cli.py`` now delegates to these functions; the JSON
worker in ``worker.py`` exposes them to the Rust shell.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass, field
from datetime import date, datetime, time, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

from .config import AppConfig, ConfigError, configuration_issues, load_config
from .fixtures import load_fixture
from .models import Outcome, SyncPlan
from .planner import build_plan
from .state import SyncState
from .sync import BLOCKERS, plan_digest

#: Protocol version spoken by the JSON worker. Bump on breaking changes.
WORKER_PROTOCOL_VERSION = 1


@dataclass(frozen=True)
class WeekRange:
    from_date: date
    to_date: date
    range_start: datetime
    range_end: datetime


@dataclass(frozen=True)
class FixturePreview:
    plan: SyncPlan
    digest: str
    has_conflicts: bool


@dataclass(frozen=True)
class HomeStatus:
    timezone: str
    week_start: date
    week_end: date
    config_issues: tuple[str, ...] = ()
    last_verified_at: str | None = None
    verified_steps: int = 0
    # Browser login verification lands in issue #3; until then the GUI
    # shows login state as unknown rather than inventing a status.
    login_state: str = "unknown"
    login_detail: str = field(
        default="Login via app-managed browser follows in the next step."
    )


def resolve_week(
    *,
    timezone_name: str,
    now: datetime,
    from_date: date | None = None,
    to_date: date | None = None,
) -> WeekRange:
    """Resolve the Monday-to-Sunday week for a preview or status request."""
    timezone = ZoneInfo(timezone_name)
    if now.tzinfo is None:
        raise ValueError("--now must include a UTC offset")
    local_now = now.astimezone(timezone)
    default_monday = local_now.date() - timedelta(days=local_now.weekday())
    start_date = from_date or default_monday
    end_date = to_date or (start_date + timedelta(days=6))
    if end_date < start_date:
        raise ValueError("--to must be on or after --from")
    return WeekRange(
        from_date=start_date,
        to_date=end_date,
        range_start=datetime.combine(start_date, time.min, timezone),
        range_end=datetime.combine(end_date + timedelta(days=1), time.min, timezone),
    )


def load_app_config(config_path: Path) -> AppConfig:
    return load_config(config_path)


def preview_fixture(
    *,
    config: AppConfig,
    fixture_path: Path,
    state_path: Path,
    from_date: date | None = None,
    to_date: date | None = None,
    now: datetime | None = None,
) -> FixturePreview:
    """Build a read-only fixture preview without touching external services.

    Opens its own short-lived ``SyncState`` so both the CLI and the worker
    share one code path. Never records successful steps; dry-run stays
    read-only by design.
    """
    timezone = ZoneInfo(config.timezone)
    resolved_now = now or datetime.now(timezone)
    week = resolve_week(
        timezone_name=config.timezone,
        now=resolved_now,
        from_date=from_date,
        to_date=to_date,
    )
    shifts, destination = load_fixture(fixture_path)
    with SyncState(state_path) as state:
        plan = build_plan(
            config=config,
            shifts=shifts,
            destination=destination,
            range_start=week.range_start,
            range_end=week.range_end,
            now=resolved_now,
            state=state,
        )
    return FixturePreview(
        plan=plan,
        digest=plan_digest(plan),
        has_conflicts=any(
            item.outcome.value in {"conflicted", "failed"} for item in plan.items
        ),
    )


def fixture_preview_from_paths(
    *,
    config_path: Path,
    fixture_path: Path,
    state_path: Path,
    from_date: date | None = None,
    to_date: date | None = None,
    now: datetime | None = None,
) -> FixturePreview:
    """Convenience wrapper for callers that only have file paths (CLI/worker)."""
    return preview_fixture(
        config=load_config(config_path),
        fixture_path=fixture_path,
        state_path=state_path,
        from_date=from_date,
        to_date=to_date,
        now=now,
    )


def describe_blockers(plan: SyncPlan) -> tuple[str, ...]:
    return tuple(
        f"{item.source_key} {item.step_key}: {item.summary}"
        for item in plan.items
        if item.outcome in BLOCKERS
    )


def home_status(
    *,
    config_path: Path,
    state_path: Path,
    now: datetime | None = None,
) -> HomeStatus:
    """Data for the Danish home screen: week, config health, last transfer."""
    try:
        config = load_config(config_path)
    except ConfigError:
        # A missing/unreadable config is itself the home-screen state: the
        # GUI shows setup guidance instead of crashing on startup.
        resolved_now = now or datetime.now().astimezone()
        monday = resolved_now.date() - timedelta(days=resolved_now.weekday())
        return HomeStatus(
            timezone="Europe/Copenhagen",
            week_start=monday,
            week_end=monday + timedelta(days=6),
            config_issues=("Konfigurationsfilen mangler eller er ugyldig.",),
        )
    resolved_now = now or datetime.now(ZoneInfo(config.timezone))
    week = resolve_week(
        timezone_name=config.timezone, now=resolved_now, from_date=None, to_date=None
    )
    last_verified_at, verified_steps = _last_verified_transfer(state_path)
    return HomeStatus(
        timezone=config.timezone,
        week_start=week.from_date,
        week_end=week.to_date,
        config_issues=configuration_issues(config),
        last_verified_at=last_verified_at,
        verified_steps=verified_steps,
    )


def _last_verified_transfer(state_path: Path) -> tuple[str | None, int]:
    """Return (newest verified updated_at, verified step count).

    Missing state means "never transferred" rather than an error: a fresh
    install has no SQLite file yet. Verified rows are written only by
    ``apply_plan`` after read-back, so this is the last *verified* transfer.
    """
    if not Path(state_path).exists() and str(state_path) != ":memory:":
        return None, 0
    try:
        with SyncState(state_path) as state:
            state.initialize()
            row = state.connection.execute(
                "SELECT COUNT(*) AS n, MAX(updated_at) AS latest "
                "FROM sync_steps WHERE status = 'verified'"
            ).fetchone()
    except (sqlite3.Error, OSError):
        return None, 0
    if row is None or not row["n"]:
        return None, 0
    return row["latest"], int(row["n"])


def app_data_dir(app_name: str = "teamup-shift-sync") -> Path:
    """Per-user application data directory, preserving existing SQLite state.

    Uses OS conventions (XDG on Linux, Application Support on macOS,
    %APPDATA% on Windows) so installs, upgrades, and uninstalls behave like
    a normal desktop app. Callers pass explicit ``state_path`` values derived
    from this directory; the default repo ``.local/`` path keeps working for
    CLI development.
    """
    import os
    import sys

    override = os.environ.get("TEAMUP_SHIFT_SYNC_DATA_DIR")
    if override:
        path = Path(override).expanduser()
        path.mkdir(parents=True, exist_ok=True)
        return path
    if sys.platform == "win32":
        base = Path(os.environ.get("APPDATA", str(Path.home() / "AppData" / "Roaming")))
        path = base / app_name
    elif sys.platform == "darwin":
        path = Path.home() / "Library" / "Application Support" / app_name
    else:
        base = Path(
            os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local" / "share"))
        )
        path = base / app_name
    path.mkdir(parents=True, exist_ok=True)
    return path


def default_state_path(data_dir: Path | None = None) -> Path:
    base = data_dir or app_data_dir()
    return base / "sync.sqlite3"


def startup_issues(config: AppConfig) -> tuple[str, ...]:
    """Local completeness check without service requests (no secrets printed)."""
    issues = configuration_issues(config)
    if config.default_helper_count != 1:
        return issues
    return issues


def plan_summaries_by_outcome(plan: SyncPlan) -> dict[str, int]:
    counts: dict[str, int] = {}
    for item in plan.items:
        key = (
            item.outcome.value
            if isinstance(item.outcome, Outcome)
            else str(item.outcome)
        )
        counts[key] = counts.get(key, 0) + 1
    return counts
