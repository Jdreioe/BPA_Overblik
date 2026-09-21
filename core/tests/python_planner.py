"""Temporary parity oracle for Rust planning, using synthetic accounts only.

Capture the existing Python planner tests and additional destination/recovery
combinations. This executes the current implementation, not a copied planner.
"""

import dataclasses
import json
import sys
import unittest
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[2] / "tests"))
import test_planner
from helpers import config, moment, source_shift

from teamup_shift_sync import planner
from teamup_shift_sync.config import load_config
from teamup_shift_sync.fixtures import load_fixture
from teamup_shift_sync.models import (
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    TimeInterval,
)
from teamup_shift_sync.state import SyncState
from teamup_shift_sync.sync import plan_digest

cases = []


def capture(**kwargs):
    result = planner.build_plan(**kwargs)
    cases.append(
        {
            "config": dataclasses.asdict(kwargs["config"]),
            "shifts": [dataclasses.asdict(s) for s in kwargs["shifts"]],
            "destination": dataclasses.asdict(kwargs["destination"]),
            "range_start": kwargs["range_start"],
            "range_end": kwargs["range_end"],
            "now": kwargs["now"],
            "live": kwargs.get("live", False),
            "records": [
                dataclasses.asdict(r)
                for s in kwargs["shifts"]
                for r in kwargs["state"].steps_for_source(s.key)
            ],
            "expected": dataclasses.asdict(result),
            "digest": plan_digest(result),
        }
    )
    return result


test_planner.build_plan = capture
suite = unittest.defaultTestLoader.loadTestsFromModule(test_planner)
result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
assert result.wasSuccessful()

start = moment("2026-09-14T00:00:00+02:00")
end = moment("2026-09-21T00:00:00+02:00")
now = moment("2026-09-20T20:00:00+02:00")
base = source_shift(comment=None, notes="uni 8-10")
interval = TimeInterval(
    moment("2026-09-14T08:00:00+02:00"), moment("2026-09-14T10:00:00+02:00")
)
other_interval = dataclasses.replace(
    interval, ends_at=moment("2026-09-14T11:00:00+02:00")
)
mithf = MitHfShift("mithf", base.starts_at, base.ends_at, 1, "Mit Helper")
duos = DuosRegistration(
    "duos", "arrangement", "123", "Almindelig", interval.starts_at, interval.ends_at
)


def run(shifts=(base,), destination=None, records=(), cfg=None, live=True):
    with SyncState(":memory:") as state:
        for step, status, destination_id, payload in records:
            state.record_step(
                source_key=shifts[0].key,
                step_key=step,
                status=status,
                destination_id=destination_id,
                synced_payload=payload,
                source_hash="old-source",
                updated_at=now,
            )
        capture(
            config=cfg or config(),
            shifts=shifts,
            destination=destination or DestinationSnapshot(),
            range_start=start,
            range_end=end,
            now=now,
            state=state,
            live=live,
        )


# Exercise identity, overlap, manual edits, uncertain writes and status checks.
for system, original, step, payload in (
    (
        "mithf",
        mithf,
        "mithf.create_shift",
        {
            "starts_at": base.starts_at.isoformat(),
            "ends_at": base.ends_at.isoformat(),
            "helper_count": 1,
        },
    ),
    (
        "duos",
        duos,
        "duos.interval:notes:0",
        {
            k: v
            for k, v in dataclasses.asdict(duos).items()
            if k not in {"id", "status_id"}
        },
    ),
):
    payload = {
        k: v.isoformat() if isinstance(v, datetime) else v for k, v in payload.items()
    }
    variants = [
        (),
        (original,),
        (original, dataclasses.replace(original, id="duplicate")),
        (dataclasses.replace(original, ends_at=moment("2026-09-14T12:00:00+02:00")),),
    ]
    if system == "duos":
        variants += [
            (dataclasses.replace(duos, status_id=status),) for status in (1, 2, 5, 6)
        ]
        variants += [
            (dataclasses.replace(duos, employee_number="other"),),
            (dataclasses.replace(duos, arrangement_id="other"),),
        ]
    for candidates in variants:
        destination = DestinationSnapshot(
            **{
                "mithf_shifts"
                if system == "mithf"
                else "duos_registrations": candidates
            }
        )
        for status, destination_id, stored in (
            ("verified", original.id, payload),
            ("uncertain", None, payload),
            ("uncertain", original.id, payload),
            ("verified", "missing", payload),
            ("verified", original.id, {}),
        ):
            run(
                destination=destination,
                records=((step, status, destination_id, stored),),
            )
        run(destination=destination)
    # Source edit with destination equal to the saved payload.
    changed = dataclasses.replace(
        base, ends_at=moment("2026-09-14T16:00:00+02:00"), notes="uni 8-11"
    )
    destination = DestinationSnapshot(
        **{"mithf_shifts" if system == "mithf" else "duos_registrations": (original,)}
    )
    run(
        shifts=(changed,),
        destination=destination,
        records=((step, "verified", original.id, payload),),
    )

for helper in (None, "Mit Helper", "Other Helper", ""):
    for actual in ((), (interval,), (other_interval,)):
        for status in (None, "verified", "uncertain"):
            records = (
                ()
                if status is None
                else (
                    (
                        "mithf.assign_helper",
                        status,
                        "mithf",
                        {"helper_name": "Mit Helper"},
                    ),
                    (
                        "mithf.set_sps",
                        status,
                        "mithf",
                        {
                            "intervals": [
                                {
                                    "starts_at": interval.starts_at.isoformat(),
                                    "ends_at": interval.ends_at.isoformat(),
                                }
                            ]
                        },
                    ),
                )
            )
            for live in (False, True):
                run(
                    destination=DestinationSnapshot(
                        mithf_shifts=(
                            dataclasses.replace(
                                mithf, helper_name=helper, sps_intervals=actual
                            ),
                        )
                    ),
                    records=records,
                    live=live,
                )

for title in ("p-møde", "Husk at checke...", "Ordinary"):
    for notes in ("", "uni 8-10", "uni 8-10 & 13-14", "uni 8-10 & 13-14 & later"):
        shift = dataclasses.replace(base, title=title, notes=notes)
        for actual in ((), (TimeInterval(base.starts_at, base.ends_at),), (interval,)):
            for status in (None, "uncertain"):
                records = (
                    ()
                    if status is None
                    else (("mithf.set_meeting", status, "mithf", {}),)
                )
                run(
                    shifts=(shift,),
                    destination=DestinationSnapshot(
                        mithf_shifts=(
                            dataclasses.replace(mithf, meeting_intervals=actual),
                        )
                    ),
                    records=records,
                )

run(
    records=(
        ("mithf.create_shift#1", "verified", "retired", {}),
        ("mithf.create_shift#2", "uncertain", None, {}),
        ("mithf.set_sps", "verified", "mithf", {"intervals": []}),
        ("duos.interval:removed:0", "verified", "duos", {}),
    ),
    shifts=(dataclasses.replace(base, notes=""),),
)
for key in ("unknown", "O'Brien", 'a"b', "a\n\\b", "æøå 😀"):
    run(shifts=(dataclasses.replace(base, helper_key=key),))
run(cfg=dataclasses.replace(config(), duos_arrangement_id=""))
run(cfg=dataclasses.replace(config(), duos_registration_type=""))
run(shifts=(dataclasses.replace(base, notes="uni 8-10 & 8-10"),))
run(shifts=(dataclasses.replace(base, starts_at=now, ends_at=end, notes="uni 21-22"),))
# Exclusive range edges, stable ordering, overnight and microsecond payloads.
run(
    shifts=(
        dataclasses.replace(base, ends_at=start),
        dataclasses.replace(base, starts_at=end),
    )
)
run(shifts=(dataclasses.replace(base, event_id="second"), base))
run(
    shifts=(
        dataclasses.replace(
            base,
            starts_at=moment("2026-09-13T22:00:00+02:00"),
            ends_at=moment("2026-09-14T07:00:00+02:00"),
            notes="uni 2026-09-13 23-1",
        ),
    )
)
run(
    shifts=(
        dataclasses.replace(
            base, starts_at=moment("2026-09-14T07:30:00.120000+00:00"), notes=""
        ),
    )
)

root = Path(__file__).parents[2]
fixture_shifts, fixture_destination = load_fixture(
    root / "fixtures/representative-week.json"
)
for live in (False, True):
    run(
        shifts=fixture_shifts,
        destination=fixture_destination,
        cfg=load_config(root / "fixtures/offline-config.toml"),
        live=live,
    )

print(
    json.dumps(
        cases,
        default=lambda value: (
            value.isoformat() if isinstance(value, datetime) else str(value)
        ),
    )
)
