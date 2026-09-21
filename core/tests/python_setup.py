"""Temporary parity oracle for the Rust setup flow, synthetic data only.

Drives the current Python ``Setup`` through a scripted sequence of stages and
prints the resulting document after each one. The two catalog readers are
replaced with fixed responses, so the comparison is about the state machine —
which choices survive a refresh, what gets proposed, what resets — and not
about service access. No network, browser, keyring or real account is used.

Also prints the account scope Python derives for a confirmed document. That
scope names ``sync-<scope>.sqlite3``; if Rust computed a different one it
would orphan an existing transfer history and re-submit completed work.
"""

import hashlib
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[2] / "src"))

from teamup_shift_sync.setup import Setup, SetupError

CALENDARS = [
    {"id": "c1", "name": "Ida Å"},
    {"id": "c2", "name": "Bo Jensen"},
    {"id": "c3", "name": "Delt kalender"},
]
COLORS = {"c1": 5, "c2": 12}
RENAMED = [
    {"id": "c1", "name": "Ida Åberg"},
    {"id": "c2", "name": "Bo Jensen"},
    {"id": "c3", "name": "Delt kalender"},
]

MITHF = [
    {"id": "m1", "name": "Ida Å"},
    {"id": "m2", "name": "Bo Jensen"},
    {"id": "m3", "name": "Carl Nielsen"},
]
DUOS = [{"id": "d1", "name": "Ida Å"}, {"id": "d2", "name": "Bo Jensen"}]
ACCOUNT_IDS = {"customer": "k-7", "grant": "b-3"}


def catalog(arrangement=""):
    """The same shape ``Setup.destination_catalog`` returns."""
    result = {
        "mithf": MITHF,
        "arrangements": [
            {"id": "a1", "name": "SPS Nord"},
            {"id": "a2", "name": "SPS Syd"},
        ],
        "types": [],
        "duos": [],
        "account": "MitHF: Kunde · Bevilling",
        "account_ids": ACCOUNT_IDS,
    }
    if arrangement:
        result["types"] = [{"id": "t1", "name": "SPS-timer"}]
        result["duos"] = DUOS
    return result


def single_arrangement_catalog(arrangement=""):
    result = catalog(arrangement)
    result["arrangements"] = [{"id": "a1", "name": "SPS Nord"}]
    return result


def ambiguous_catalog(arrangement=""):
    """Two MitHF helpers share a name, so neither can be reconciled by name."""
    result = catalog(arrangement)
    result["mithf"] = MITHF + [{"id": "m4", "name": "Bo Jensen"}]
    return result


def changed_catalog(arrangement=""):
    result = catalog(arrangement)
    result["mithf"] = [{"id": "m1", "name": "Ida Å"}, {"id": "m2", "name": "Bo J."}]
    return result


def setup(directory, source=CALENDARS, colors=COLORS, destination=catalog):
    """A Setup whose two catalog readers return fixed responses."""
    instance = Setup(Path(directory), vault={})
    instance.source_catalog = lambda _secret: (
        source,
        colors,
        "Vagter og kommentarer er kontrolleret for denne uge.",
    )
    instance.destination_catalog = destination
    instance.data["credential"] = "test-credential"
    return instance


def outcome(instance, call):
    """Run one step and record the call, its error and the resulting document.

    Every step is recorded, including preparation, so the Rust replay starts
    from the same document rather than a silently prepared one.
    """
    error = None
    try:
        if call["op"] == "refresh":
            instance.refresh_source()
        elif call["op"] == "discover":
            instance.discover(call["arrangement"])
        elif call["op"] == "edit":
            instance.edit(call["params"])
        elif call["op"] == "validate":
            instance.validate_choices()
        else:
            raise AssertionError(call["op"])
    except SetupError as failure:
        error = str(failure)
    # Copy: later steps keep mutating the same document.
    return {"call": call, "error": error, "data": json.loads(json.dumps(instance.data))}


REFRESH = {"op": "refresh"}
VALIDATE = {"op": "validate"}


def discover(arrangement=None):
    return {"op": "discover", "arrangement": arrangement}


def edit(**params):
    return {"op": "edit", "params": params}


def run(name, instance, calls):
    return {
        "scenario": name,
        "steps": [outcome(instance, call) for call in calls],
    }


def scenario_refresh_then_discover(directory):
    return run(
        "refresh_then_discover",
        setup(directory),
        [
            REFRESH,
            # Two arrangements on offer, so neither may be chosen automatically.
            discover(),
            discover("a1"),
            edit(source="c1", mithf="m1", duos="d1"),
            edit(source="c2", mithf="m2", duos="d2"),
            edit(source="c3", excluded=True),
            VALIDATE,
            # An unchanged catalog must preserve the confirmed edits above.
            discover("a1"),
        ],
    )


def scenario_ambiguous_and_incomplete_choices(directory):
    return run(
        "ambiguous_and_incomplete_choices",
        setup(directory, destination=ambiguous_catalog),
        [
            REFRESH,
            discover("a1"),
            # "Bo Jensen" is ambiguous in MitHF, so nothing was proposed for it
            # and confirmation must refuse the half-reviewed document.
            VALIDATE,
            edit(source="c1", mithf="m1", duos="d1"),
            # Choosing the duplicate name explicitly is still unsafe.
            edit(source="c2", mithf="m2", duos="d2"),
            edit(source="c3", excluded=True),
            VALIDATE,
        ],
    )


def scenario_changed_catalog_resets_mappings(directory):
    instance = setup(directory)
    prepared = [
        outcome(instance, REFRESH),
        outcome(instance, discover("a1")),
        outcome(instance, edit(source="c1", mithf="m1", duos="d1")),
    ]
    instance.destination_catalog = changed_catalog
    return {
        "scenario": "changed_catalog_resets_mappings",
        "steps": prepared + [outcome(instance, discover("a1")), outcome(instance, VALIDATE)],
    }


def scenario_renamed_calendar_resets_mappings(directory):
    instance = setup(directory)
    prepared = [
        outcome(instance, REFRESH),
        outcome(instance, discover("a1")),
        outcome(instance, edit(source="c1", mithf="m1", duos="d1")),
    ]
    instance.source_catalog = lambda _secret: (
        RENAMED,
        COLORS,
        "Vagter og kommentarer er kontrolleret for denne uge.",
    )
    return {
        "scenario": "renamed_calendar_resets_mappings",
        "steps": prepared + [outcome(instance, REFRESH)],
    }


def scenario_single_arrangement_is_automatic(directory):
    return run(
        "single_arrangement_is_automatic",
        setup(directory, destination=single_arrangement_catalog),
        [REFRESH, discover()],
    )


def account_scope():
    """The scope Python derives for a confirmed document."""
    identity = {
        "account_ids": ACCOUNT_IDS,
        "arrangement": "a1",
        "registration_type": "t1",
        "mappings": [
            {"source": "c1", "mithf": "m1", "duos": "d1", "excluded": False},
            {"source": "c2", "mithf": "m2", "duos": "d2", "excluded": False},
            {"source": "c3", "mithf": "", "duos": "", "excluded": True},
        ],
        "calendar": "ksAbc123",
    }
    digest = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    return {"identity": identity, "scope": digest[:24]}


def main():
    scenarios = []
    for build in (
        scenario_refresh_then_discover,
        scenario_ambiguous_and_incomplete_choices,
        scenario_changed_catalog_resets_mappings,
        scenario_renamed_calendar_resets_mappings,
        scenario_single_arrangement_is_automatic,
    ):
        with tempfile.TemporaryDirectory() as directory:
            scenarios.append(build(directory))
    json.dump(
        {"scenarios": scenarios, "account_scope": account_scope()},
        sys.stdout,
        ensure_ascii=False,
    )


if __name__ == "__main__":
    main()
