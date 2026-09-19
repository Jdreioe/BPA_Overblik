from __future__ import annotations

import hashlib
import json
from collections.abc import Callable
from datetime import datetime

from .browser import DestinationError
from .destinations import Destinations
from .models import (
    AppConfig,
    DestinationSnapshot,
    Outcome,
    PlanItem,
    SourceShift,
    SyncPlan,
)
from .planner import build_plan
from .state import SyncState

WRITES = {Outcome.WOULD_CREATE, Outcome.WOULD_UPDATE}
BLOCKERS = {
    Outcome.CONFLICTED,
    Outcome.REVIEW,
    Outcome.FAILED,
    Outcome.PENDING_INTEGRATION,
    Outcome.PENDING_MITHF_BUG,
}


def reconciliation_range(
    shifts: tuple[SourceShift, ...], start: datetime, end: datetime
) -> tuple[datetime, datetime]:
    """Read full selected shifts, including hours outside the selected week."""
    selected = [s for s in shifts if s.ends_at > start and s.starts_at < end]
    return (
        min([start, *(s.starts_at for s in selected)]),
        max([end, *(s.ends_at for s in selected)]),
    )


def plan_digest(plan: SyncPlan) -> str:
    """Bind approval to dates, resolved actions, destination IDs, and values."""
    value = {
        "from": plan.starts_at.isoformat(),
        "to": plan.ends_at.isoformat(),
        "items": [
            {
                "source": i.source_key,
                "step": i.step_key,
                "outcome": i.outcome,
                "destination": i.destination_id,
                "payload": i.payload,
            }
            for i in plan.items
        ],
    }
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def apply_plan(*, state: SyncState, **kwargs) -> SyncPlan:
    with state.exclusive_apply():
        return _apply_plan(state=state, **kwargs)


def _apply_plan(
    *,
    config: AppConfig,
    shifts: tuple[SourceShift, ...],
    destinations: Destinations,
    state: SyncState,
    start: datetime,
    end: datetime,
    expected_digest: str,
    now: datetime,
    progress: Callable[[str], None] = print,
) -> SyncPlan:
    """Reconcile every step; never retry an ambiguous create automatically."""

    def plan(snapshot: DestinationSnapshot) -> SyncPlan:
        return build_plan(
            config=config,
            shifts=shifts,
            destination=snapshot,
            range_start=start,
            range_end=end,
            now=now,
            state=state,
            live=True,
        )

    read_start, read_end = reconciliation_range(shifts, start, end)
    snapshot = destinations.read(read_start, read_end)
    approved = plan(snapshot)
    if plan_digest(approved) != expected_digest:
        raise DestinationError(
            "The plan changed. Review a fresh dry-run before applying it"
        )
    if any(i.outcome in BLOCKERS for i in approved.items):
        raise DestinationError(
            "The selected batch has unresolved items; no writes were made"
        )
    approved_by_key = {(i.source_key, i.step_key): i for i in approved.items}
    # Detect a source batch attempting to use one destination for different shifts.
    claims: dict[tuple[str, str], str] = {}
    for item in approved.items:
        if item.destination_id and item.step_key == "mithf.create_shift":
            key = (item.system, item.destination_id)
            if key in claims and claims[key] != item.source_key:
                raise DestinationError(
                    "Multiple source shifts claim the same MitHF shift"
                )
            claims[key] = item.source_key
    sources = {s.key: s for s in shifts}
    for original in approved.items:
        if original.outcome == Outcome.EXCLUDED:
            continue
        snapshot = destinations.read(read_start, read_end)
        current = plan(snapshot)
        item = next(
            (
                i
                for i in current.items
                if (i.source_key, i.step_key)
                == (original.source_key, original.step_key)
            ),
            None,
        )
        if item is None or item.payload != original.payload or item.outcome in BLOCKERS:
            raise DestinationError(
                "Destination changed during synchronization; stopped before the next write"
            )
        if item.outcome == Outcome.ALREADY_MATCHED:
            _record(state, item, "verified", item.destination_id)
            continue
        if (
            item.outcome not in WRITES
            or approved_by_key[(item.source_key, item.step_key)].outcome not in WRITES
        ):
            raise DestinationError("A new unapproved write appeared; stopped")
        # The durable marker covers both a timeout and a process crash after submission.
        _record(state, item, "uncertain", item.destination_id)
        id_ = destinations.write(item, snapshot)
        if id_:
            _record(state, item, "uncertain", id_)
        if item.step_key == "mithf.assign_helper" and id_:
            parent = state.get_step(item.source_key, "mithf.create_shift")
            if parent:
                state.record_step(
                    source_key=item.source_key,
                    step_key=parent.step_key,
                    status="verified",
                    destination_id=id_,
                    source_hash=parent.source_hash,
                    synced_payload=parent.synced_payload,
                    updated_at=datetime.now(now.tzinfo),
                )
        verified_plan = plan(destinations.read(read_start, read_end))
        verified = next(
            (
                i
                for i in verified_plan.items
                if (i.source_key, i.step_key) == (item.source_key, item.step_key)
            ),
            None,
        )
        if verified is None or verified.outcome != Outcome.ALREADY_MATCHED:
            raise DestinationError(
                "Saved values did not match the plan; stopped for reconciliation"
            )
        _record(state, verified, "verified", verified.destination_id)
        progress(f"Verified {item.step_key}")
    final = plan(destinations.read(read_start, read_end))
    if any(
        i.outcome not in {Outcome.ALREADY_MATCHED, Outcome.EXCLUDED}
        for i in final.items
    ):
        raise DestinationError("Final reconciliation found unresolved changes")
    for key in {
        i.source_key for i in final.items if i.outcome == Outcome.ALREADY_MATCHED
    }:
        state.record_source_snapshot(sources[key], datetime.now(now.tzinfo))
    return final


def _record(
    state: SyncState, item: PlanItem, status: str, destination_id: str | None
) -> None:
    state.record_step(
        source_key=item.source_key,
        step_key=item.step_key,
        status=status,
        destination_id=destination_id or None,
        source_hash=hashlib.sha256(
            json.dumps(item.payload, sort_keys=True).encode()
        ).hexdigest(),
        synced_payload=item.payload,
        updated_at=datetime.now().astimezone(),
    )
