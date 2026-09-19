from __future__ import annotations

import os
import tomllib
from pathlib import Path

from .models import AppConfig, HelperMapping
from .teamup import subcalendar_color


class ConfigError(ValueError):
    pass


def load_config(path: Path) -> AppConfig:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ConfigError(f"Configuration file not found: {path}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"Invalid TOML in {path}: {error}") from error

    helpers: dict[str, HelperMapping] = {}
    colors: dict[str, str] = {}
    for raw in data.get("helpers", []):
        mapping = HelperMapping(
            teamup_key=_required(raw, "teamup_key", "helpers"),
            teamup_display_name=_required(raw, "teamup_display_name", "helpers"),
            mithf_name=_required(raw, "mithf_name", "helpers"),
            duos_name=_required(raw, "duos_name", "helpers"),
            duos_employee_number=_required(raw, "duos_employee_number", "helpers"),
        )
        if mapping.teamup_key in helpers:
            raise ConfigError(f"Duplicate TeamUp helper key: {mapping.teamup_key}")
        helpers[mapping.teamup_key] = mapping
        # Optional: the helper's Teamup colour id, so a config-driven run
        # shows the same week colours the guided setup reads automatically.
        color = raw.get("teamup_color")
        if color is not None:
            if type(color) is not int or not 1 <= color <= 48:
                raise ConfigError(
                    "helpers.teamup_color must be a Teamup colour id 1-48"
                )
            colors[mapping.teamup_key] = subcalendar_color(color)

    teamup = data.get("teamup", {})
    duos = data.get("duos", {})

    subcalendar_helpers = teamup.get("helper_subcalendars", {})
    if not isinstance(subcalendar_helpers, dict) or any(
        not key.isdigit() or not isinstance(value, str) or not value.strip()
        for key, value in subcalendar_helpers.items()
    ):
        raise ConfigError(
            "teamup.helper_subcalendars must map numeric IDs to helper names"
        )
    helper_count = data.get("default_helper_count", 1)
    if not isinstance(helper_count, int) or helper_count < 1:
        raise ConfigError("default_helper_count must be a positive integer")
    lookback_days = teamup.get("lookback_days", 7)
    if type(lookback_days) is not int or lookback_days < 1:
        raise ConfigError("teamup.lookback_days must be a positive integer")

    return AppConfig(
        timezone=str(data.get("timezone", "Europe/Copenhagen")),
        default_helper_count=helper_count,
        teamup_calendar_key=str(
            teamup.get("calendar_key") or os.environ.get("TEAMUP_CALENDAR_KEY", "")
        ),
        teamup_api_key=str(teamup.get("api_key") or os.environ.get("TEAMUP_API", "")),
        teamup_bearer_token=str(
            teamup.get("bearer_token") or os.environ.get("TEAMUP_BEARER_TOKEN", "")
        ),
        teamup_helper_field=str(teamup.get("helper_field", "")),
        duos_arrangement_id=str(duos.get("arrangement_id", "")),
        duos_registration_type=str(duos.get("registration_type", "")),
        helpers=helpers,
        teamup_subcalendar_colors=colors,
        teamup_subcalendar_helpers=subcalendar_helpers,
        teamup_lookback_days=lookback_days,
    )


def configuration_issues(config: AppConfig) -> tuple[str, ...]:
    """Check local completeness without printing credentials or helper identities.

    Passing these checks does not validate identities against the live services.
    """
    issues: list[str] = []
    if config.default_helper_count != 1:
        issues.append("Live synchronization requires one helper per source shift")
    if not config.teamup_calendar_key:
        issues.append("TeamUp calendar key is missing")
    if not config.teamup_api_key:
        issues.append("TeamUp API key is missing")
    if not config.teamup_helper_field:
        issues.append("TeamUp helper field is missing")
    if config.teamup_helper_field == "subcalendar":
        source = config.teamup_subcalendar_helpers
        if not source:
            issues.append("TeamUp helper subcalendar mappings are missing")
        missing = source.keys() - config.helpers.keys()
        if missing:
            issues.append(f"{len(missing)} source helpers lack destination mappings")
        mismatched = sum(
            source[key] != mapping.teamup_display_name
            for key, mapping in config.helpers.items()
            if key in source
        )
        if mismatched:
            issues.append(
                f"{mismatched} destination mappings disagree with source names"
            )
    if not config.helpers:
        issues.append("Destination helper mappings are missing")
    for attribute, label in (
        ("mithf_name", "MitHF helper"),
        ("duos_employee_number", "DUOS employee"),
    ):
        values = [getattr(mapping, attribute) for mapping in config.helpers.values()]
        if len(set(values)) != len(values):
            issues.append(f"Multiple source helpers map to the same {label}")
    if not config.duos_arrangement_id:
        issues.append("DUOS arrangement is not configured")
    if not config.duos_registration_type:
        issues.append("DUOS registration type is not configured")
    return tuple(issues)


def _required(raw: dict[str, object], key: str, section: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ConfigError(f"{section}.{key} must be a non-empty string")
    return value.strip()
