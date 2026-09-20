"""Temporary migration oracle; uses only synthetic data in a test-owned directory."""

import json
import sys
from datetime import datetime
from pathlib import Path

from teamup_shift_sync.models import SourceComment, SourceShift
from teamup_shift_sync.state import SyncState

mode, path = sys.argv[1:]
now = datetime.fromisoformat("2026-09-20T20:00:00.120000+02:00")

if mode == "create":
    fixture = Path(__file__).parents[2] / "fixtures/representative-week.json"
    shifts = json.loads(fixture.read_text(encoding="utf-8"))["source_shifts"]
    shifts[0]["title"] = 'Vagtmøde æøå 😀\x7f\n"\\'
    shifts[0]["recurrence_start"] = "2026-09-14T07:30:00.120000+02:00"
    shifts[0]["source_version"] = "version-1"
    shifts[0]["comments"][0]["updated_at"] = "2026-09-14T06:00:00.000001+00:00"
    # Both null and timestamp comment hashes must remain stable.
    shifts[0]["comments"].append({"id": "null-time", "text": "æøå 😀"})
    with SyncState(path) as state:
        for index, raw in enumerate(shifts):
            values = dict(raw)
            for field in ("starts_at", "ends_at", "recurrence_start"):
                if values.get(field):
                    values[field] = datetime.fromisoformat(values[field])
            values["comments"] = tuple(
                SourceComment(
                    c["id"],
                    c["text"],
                    datetime.fromisoformat(c["updated_at"])
                    if c.get("updated_at")
                    else None,
                )
                for c in values.get("comments", [])
            )
            shift = SourceShift(**values)
            state.record_source_snapshot(shift, now)
            state.record_step(
                source_key=shift.key,
                step_key="mithf.create_shift",
                status="uncertain" if index == 0 else "verified",
                destination_id=None if index == 0 else str(index),
                source_hash="existing-hash",
                synced_payload={"helper": "æøå", "count": 1},
                error="read-back interrupted" if index == 0 else None,
                updated_at=now,
            )
    print(json.dumps(shifts))
elif mode == "check":
    with SyncState(path) as state:
        record = state.get_step("rust-source", "duos.register")
        assert record.status == "uncertain"
        assert record.destination_id is None
        assert record.synced_payload == {"name": "æøå", "intervals": [1, 2]}
        assert record.source_hash == "rust-hash"
elif mode == "try-lock":
    with SyncState(path) as state:
        try:
            with state.exclusive_apply():
                pass
        except ValueError:
            sys.exit(23)
elif mode == "hold-lock":
    with SyncState(path) as state, state.exclusive_apply():
        print("locked", flush=True)
        sys.stdin.readline()
else:
    raise ValueError("Unknown test mode")
