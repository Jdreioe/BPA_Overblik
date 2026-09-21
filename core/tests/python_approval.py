"""Temporary approval oracle: capture real Python apply runs and hashes."""

import dataclasses
import json
import sys
import unittest
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[2] / "tests"))
import test_sync

from teamup_shift_sync import sync
from teamup_shift_sync.models import Outcome, PlanItem, SyncPlan
from teamup_shift_sync.state import SyncState

traces = []
original_apply = sync.apply_plan
original_build = sync.build_plan


def capture_apply(**kwargs):
    destination = kwargs["destinations"]
    state = kwargs["state"]
    shifts = kwargs["shifts"]
    writes_before = len(destination.writes)
    trace = {
        "digest": kwargs["expected_digest"],
        "plans": [],
        "error": None,
        "config": dataclasses.asdict(kwargs["config"]),
        "shifts": [dataclasses.asdict(s) for s in shifts],
        "start": kwargs["start"],
        "end": kwargs["end"],
        "now": kwargs["now"],
        "destination_before": dataclasses.asdict(destination.snapshot),
        "timeout_after_create": destination.timeout_after_create,
        "records_before": [
            dataclasses.asdict(r) for s in shifts for r in state.steps_for_source(s.key)
        ],
    }

    def capture_plan(**plan_kwargs):
        plan = original_build(**plan_kwargs)
        trace["plans"].append(dataclasses.asdict(plan))
        return plan

    sync.build_plan = capture_plan
    try:
        return original_apply(**kwargs)
    except Exception as error:
        trace["error"] = str(error)
        raise
    finally:
        sync.build_plan = original_build
        trace["destination_after"] = dataclasses.asdict(destination.snapshot)
        trace["records_after"] = [
            dataclasses.asdict(r) for s in shifts for r in state.steps_for_source(s.key)
        ]
        trace["writes"] = destination.writes[writes_before:]
        traces.append(trace)


test_sync.apply_plan = capture_apply
suite = unittest.defaultTestLoader.loadTestsFromModule(test_sync)
result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
assert result.wasSuccessful()

moment = datetime.fromisoformat("2026-09-20T20:00:00.120000+00:00")
# Nested JSON covers object ordering, nulls, booleans, all ASCII controls,
# supplementary Unicode and the full integer range accepted by Rust JSON.
item = PlanItem(
    "source:æøå😀",
    "mithf",
    "mithf.set_meeting#1",
    Outcome.WOULD_CREATE,
    "not hashed",
    {
        "z": None,
        "a": [True, False, [], {}, {"😀": "emoji", "\uffff": "bmp"}],
        "text": 'Vagtmøde æøå 😀"\\' + "".join(chr(i) for i in range(128)),
        "limits": [-9223372036854775808, 18446744073709551615],
    },
)
plan = SyncPlan(moment, moment, moment, (item,))
with SyncState(":memory:") as state:
    sync._record(state, item, "uncertain", None)
    payload_hash = state.get_step(item.source_key, item.step_key).source_hash
print(
    json.dumps(
        {
            "traces": traces,
            "hash_case": {
                "plan": dataclasses.asdict(plan),
                "digest": sync.plan_digest(plan),
                "payload_hash": payload_hash,
            },
        },
        default=lambda value: (
            value.isoformat() if isinstance(value, datetime) else str(value)
        ),
    )
)
