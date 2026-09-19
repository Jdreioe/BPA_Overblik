"""App-owned Python worker: versioned JSON protocol over stdin/stdout.

The Rust shell spawns exactly one worker and speaks newline-delimited JSON.
Stdout carries *only* protocol frames; everything diagnostic goes to stderr
so a stray log line can never corrupt the protocol stream.

Protocol v1 frames::

    request  {"protocol": 1, "id": "<opaque>", "method": "<name>", "params": {...}}
    result   {"protocol": 1, "id": "<opaque>", "event": "result", "payload": {...}}
    progress {"protocol": 1, "id": "<opaque>", "event": "progress", "payload": {...}}
    error    {"protocol": 1, "id": "<opaque>", "event": "error",
              "payload": {"code": "<stable>", "message": "<Danish>", "detail": "<en>"}}

Methods: ``ping``, ``app_paths``, ``home_status``, ``preview_fixture``.
Apply/progress streaming follows in issue #6 and reuses the same envelope.
"""

from __future__ import annotations

import json
import sys
import traceback
from datetime import date, datetime
from pathlib import Path
from typing import Any

from . import operations
from .config import ConfigError
from .fixtures import FixtureError
from .models import Outcome, SyncPlan

PROTOCOL_VERSION = operations.WORKER_PROTOCOL_VERSION

ERROR_DANISH: dict[str, str] = {
    "bad_request": "Ugyldig forespørgsel til baggrundsarbejderen.",
    "bad_protocol": "Baggrundsarbejderen forstår ikke denne protokolversion.",
    "config_error": "Konfigurationen mangler eller er ugyldig.",
    "fixture_error": "Testugen kunne ikke indlæses.",
    "internal": "Der opstod en uventet fejl i baggrundsarbejderen.",
}


def plan_to_dict(plan: SyncPlan) -> dict[str, Any]:
    return {
        "starts_at": plan.starts_at.isoformat(),
        "ends_at": plan.ends_at.isoformat(),
        "generated_at": plan.generated_at.isoformat(),
        "digest": "",
        "items": [plan_item_to_dict(item) for item in plan.items],
    }


def plan_item_to_dict(item: Any) -> dict[str, Any]:
    outcome = (
        item.outcome.value if isinstance(item.outcome, Outcome) else str(item.outcome)
    )
    return {
        "source_key": item.source_key,
        "system": item.system,
        "step_key": item.step_key,
        "outcome": outcome,
        "summary": item.summary,
        "payload": item.payload,
        "destination_id": item.destination_id,
    }


def _parse_date(value: Any) -> date | None:
    if value in (None, ""):
        return None
    return date.fromisoformat(str(value))


def _parse_datetime(value: Any) -> datetime | None:
    if value in (None, ""):
        return None
    parsed = datetime.fromisoformat(str(value))
    if parsed.tzinfo is None:
        raise ValueError("datetime must include a UTC offset")
    return parsed


def handle_request(method: str, params: dict[str, Any]) -> dict[str, Any]:
    """Execute one request; raises ValueError/ConfigError/FixtureError."""
    if method == "ping":
        return {"version": PROTOCOL_VERSION}
    if method == "app_paths":
        data_dir = operations.app_data_dir()
        return {
            "data_dir": str(data_dir),
            "state_path": str(operations.default_state_path(data_dir)),
        }
    if method == "home_status":
        status = operations.home_status(
            config_path=Path(params["config_path"]),
            state_path=Path(
                params.get("state_path") or operations.default_state_path()
            ),
            now=_parse_datetime(params.get("now")),
        )
        return {
            "timezone": status.timezone,
            "week_start": status.week_start.isoformat(),
            "week_end": status.week_end.isoformat(),
            "config_issues": list(status.config_issues),
            "last_verified_at": status.last_verified_at,
            "verified_steps": status.verified_steps,
            "login_state": status.login_state,
            "login_detail": status.login_detail,
        }
    if method == "preview_fixture":
        preview = operations.fixture_preview_from_paths(
            config_path=Path(params["config_path"]),
            fixture_path=Path(params["fixture_path"]),
            state_path=Path(
                params.get("state_path") or operations.default_state_path()
            ),
            from_date=_parse_date(params.get("from")),
            to_date=_parse_date(params.get("to")),
            now=_parse_datetime(params.get("now")),
        )
        payload = plan_to_dict(preview.plan)
        payload["digest"] = preview.digest
        payload["has_conflicts"] = preview.has_conflicts
        payload["counts"] = operations.plan_summaries_by_outcome(preview.plan)
        payload["blockers"] = list(operations.describe_blockers(preview.plan))
        return payload
    raise ValueError(f"unknown method: {method}")


def error_payload(code: str, detail: str = "") -> dict[str, Any]:
    return {
        "code": code,
        "message": ERROR_DANISH.get(code, ERROR_DANISH["internal"]),
        "detail": detail,
    }


def classify_error(error: BaseException) -> tuple[str, str]:
    # ConfigError/FixtureError subclass ValueError, so check them first.
    if isinstance(error, ConfigError):
        return "config_error", str(error)
    if isinstance(error, FixtureError):
        return "fixture_error", str(error)
    if isinstance(error, (ValueError, KeyError)):
        return "bad_request", str(error)
    return "internal", f"{type(error).__name__}: {error}"


def process_line(line: str) -> str | None:
    """Process one stdin line into a stdout frame. Never raises."""
    try:
        request = json.loads(line)
    except json.JSONDecodeError as error:
        return json.dumps(
            {
                "protocol": PROTOCOL_VERSION,
                "id": None,
                "event": "error",
                "payload": error_payload("bad_request", f"invalid JSON: {error}"),
            },
            ensure_ascii=False,
        )
    request_id = request.get("id")
    try:
        if request.get("protocol") != PROTOCOL_VERSION:
            raise ValueError(
                f"unsupported protocol {request.get('protocol')!r}; worker speaks v{PROTOCOL_VERSION}"
            )
        method = request.get("method")
        params = request.get("params") or {}
        if not isinstance(method, str) or not isinstance(params, dict):
            # ValueError is intentional: it maps to the bad_request frame.
            raise ValueError(  # noqa: TRY004
                "request needs a string 'method' and object 'params'"
            )
        payload = handle_request(method, params)
        return json.dumps(
            {
                "protocol": PROTOCOL_VERSION,
                "id": request_id,
                "event": "result",
                "payload": payload,
            },
            ensure_ascii=False,
        )
    except Exception as error:  # noqa: BLE001 - every failure becomes an error frame
        code, detail = classify_error(error)
        if code == "bad_request" and "unsupported protocol" in detail:
            code = "bad_protocol"
        print(f"worker error [{code}]: {detail}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        # Config/fixture messages may name a local path, which is fine for
        # the app-owned Help view but never includes tokens or shift text.
        return json.dumps(
            {
                "protocol": PROTOCOL_VERSION,
                "id": request_id,
                "event": "error",
                "payload": error_payload(code, detail),
            },
            ensure_ascii=False,
        )


def main() -> int:
    for line in sys.stdin:
        if not line.strip():
            continue
        frame = process_line(line)
        if frame is not None:
            sys.stdout.write(frame + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
