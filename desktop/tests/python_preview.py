"""Temporary Rust presentation oracle; runs all existing Python preview cases."""
import dataclasses
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[2] / "tests"))
import test_preview
from teamup_shift_sync.preview import build_week_preview

cases = []

def capture(**kwargs):
    result = build_week_preview(**kwargs)
    cases.append({
        "config": dataclasses.asdict(kwargs["config"]),
        "names": kwargs["config"].teamup_subcalendar_helpers,
        "colors": kwargs["config"].teamup_subcalendar_colors,
        "shifts": [dataclasses.asdict(s) for s in kwargs["shifts"]],
        "plan": dataclasses.asdict(kwargs["plan"]),
        "destination": dataclasses.asdict(kwargs["destination"]),
        "destination_read": kwargs["destination_read"],
        "expected": dataclasses.asdict(result),
    })
    return result

test_preview.build_week_preview = capture
result = unittest.TextTestRunner(stream=sys.stderr).run(unittest.defaultTestLoader.loadTestsFromModule(test_preview))
assert result.wasSuccessful()
# Exercise day splitting when the offset changes inside an overnight shift.
from helpers import config, moment, source_shift
from teamup_shift_sync.models import DestinationSnapshot
from teamup_shift_sync.planner import build_plan
from teamup_shift_sync.state import SyncState
for start, end, week_start, week_end in [
    ("2026-03-28T22:00:00+01:00", "2026-03-29T07:00:00+02:00", "2026-03-23T00:00:00+01:00", "2026-03-30T00:00:00+02:00"),
    ("2026-10-24T22:00:00+02:00", "2026-10-25T07:00:00+01:00", "2026-10-19T00:00:00+02:00", "2026-10-26T00:00:00+01:00"),
]:
    shifts = (source_shift(starts_at=start, ends_at=end, comment=None),)
    destination = DestinationSnapshot()
    with SyncState(":memory:") as state:
        plan = build_plan(config=config(), shifts=shifts, destination=destination,
            range_start=moment(week_start), range_end=moment(week_end), now=moment(week_end), state=state, live=True)
    capture(config=config(), shifts=shifts, plan=plan, destination=destination, destination_read=True)
print(json.dumps(cases, default=lambda value: value.isoformat()))
