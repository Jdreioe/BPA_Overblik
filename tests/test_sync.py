import unittest
from dataclasses import replace

from helpers import config, moment, source_shift

from teamup_shift_sync.browser import DestinationError
from teamup_shift_sync.models import (
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    Outcome,
    TimeInterval,
    step_base,
)
from teamup_shift_sync.planner import build_plan
from teamup_shift_sync.state import SyncState
from teamup_shift_sync.sync import apply_plan, plan_digest


class MemoryDestinations:
    def __init__(self, timeout_after_create=False):
        self.snapshot = DestinationSnapshot()
        self.writes = []
        self.timeout_after_create = timeout_after_create

    def read(self, start, end):
        return self.snapshot

    def write(self, item, snapshot):
        self.writes.append(item.step_key)
        p = item.payload
        # A split shift writes several MitHF shifts, so the fake keys them by
        # the destination ID the planner resolved rather than assuming one.
        step = step_base(item.step_key)
        shifts = {shift.id: shift for shift in snapshot.mithf_shifts}
        if step == "mithf.create_shift":
            new_id = item.destination_id or f"new-{len(shifts)}"
            shifts[new_id] = MitHfShift(
                new_id, moment(p["starts_at"]), moment(p["ends_at"]), 1, None
            )
            self._store(snapshot, shifts)
            if self.timeout_after_create:
                self.timeout_after_create = False
                raise DestinationError("Response lost")
            return new_id
        if step == "mithf.assign_helper":
            # MitHF can return a different shift ID after booking.
            previous = shifts.pop(item.destination_id)
            booked = replace(
                previous,
                id=f"booked-{item.destination_id}",
                helper_name=p["helper_name"],
            )
            shifts[booked.id] = booked
            self._store(snapshot, shifts)
            return booked.id
        if step == "mithf.set_sps":
            intervals = tuple(
                TimeInterval(moment(i["starts_at"]), moment(i["ends_at"]))
                for i in p["intervals"]
            )
            shifts[item.destination_id] = replace(
                shifts[item.destination_id], sps_intervals=intervals
            )
            self._store(snapshot, shifts)
            return item.destination_id
        if item.system == "duos":
            entry = DuosRegistration(
                f"duos-{len(snapshot.duos_registrations)}",
                p["arrangement_id"],
                p["employee_number"],
                p["registration_type"],
                moment(p["starts_at"]),
                moment(p["ends_at"]),
            )
            self.snapshot = replace(
                snapshot,
                duos_registrations=tuple(
                    r
                    for r in snapshot.duos_registrations
                    if r.id != item.destination_id
                )
                + (entry,),
            )
            return ""
        raise AssertionError("Unexpected write")

    def _store(self, snapshot, shifts):
        self.snapshot = replace(
            snapshot,
            mithf_shifts=tuple(sorted(shifts.values(), key=lambda s: s.starts_at)),
        )


class SyncTests(unittest.TestCase):
    start = moment("2026-09-14T00:00:00+02:00")
    end = moment("2026-09-21T00:00:00+02:00")
    now = moment("2026-09-20T20:00:00+02:00")

    def preview(self, state, destinations, shift):
        return build_plan(
            config=config(),
            shifts=(shift,),
            destination=destinations.read(self.start, self.end),
            range_start=self.start,
            range_end=self.end,
            now=self.now,
            state=state,
            live=True,
        )

    def apply(self, state, destinations, shift, digest=None):
        return apply_plan(
            config=config(),
            shifts=(shift,),
            destinations=destinations,
            state=state,
            start=self.start,
            end=self.end,
            now=self.now,
            expected_digest=digest
            or plan_digest(self.preview(state, destinations, shift)),
            progress=lambda _: None,
        )

    def test_create_assign_sps_duos_and_repeat_without_duplicates(self):
        shift = source_shift(comment="uni 8-10")
        destination = MemoryDestinations()
        with SyncState(":memory:") as state:
            final = self.apply(state, destination, shift)
            self.assertTrue(
                all(i.outcome == Outcome.ALREADY_MATCHED for i in final.items)
            )
            self.assertEqual(
                "booked-new-0",
                state.get_step(shift.key, "mithf.create_shift").destination_id,
            )
            writes = list(destination.writes)
            self.apply(state, destination, shift)
            self.assertEqual(writes, destination.writes)
            self.assertEqual(4, len(writes))

    def test_timeout_after_creation_resumes_assignment_without_duplicate(self):
        shift = source_shift(comment=None)
        destination = MemoryDestinations(timeout_after_create=True)
        with SyncState(":memory:") as state:
            with self.assertRaises(DestinationError):
                self.apply(state, destination, shift)
            self.assertEqual(
                "uncertain", state.get_step(shift.key, "mithf.create_shift").status
            )
            self.apply(state, destination, shift)
            self.assertEqual(1, destination.writes.count("mithf.create_shift"))

    def test_unknown_create_outcome_with_no_matching_record_stays_blocked(self):
        shift = source_shift(comment=None)
        destination = MemoryDestinations(timeout_after_create=True)
        with SyncState(":memory:") as state:
            with self.assertRaises(DestinationError):
                self.apply(state, destination, shift)
            destination.snapshot = DestinationSnapshot()
            with self.assertRaisesRegex(DestinationError, "unresolved"):
                self.apply(state, destination, shift)
            self.assertEqual(1, len(destination.writes))

    def test_changed_source_invalidates_approval_before_any_write(self):
        shift = source_shift(comment=None)
        destination = MemoryDestinations()
        with SyncState(":memory:") as state:
            digest = plan_digest(self.preview(state, destination, shift))
            changed = replace(shift, ends_at=moment("2026-09-14T16:00:00+02:00"))
            with self.assertRaisesRegex(DestinationError, "plan changed"):
                self.apply(state, destination, changed, digest)
            self.assertEqual([], destination.writes)

    def test_split_sps_writes_one_mithf_shift_per_interval(self):
        """``uni 8-10 & 13-14`` becomes two consecutive MitHF shifts.

        MitHF holds one SPS interval per shift, so the 07:30-15:00 source
        shift is cut at 13:00, the start of the second interval.
        """
        destination = MemoryDestinations()
        with SyncState(":memory:") as state:
            final = self.apply(state, destination, source_shift())

            self.assertEqual(
                [
                    ("2026-09-14T07:30:00+02:00", "2026-09-14T13:00:00+02:00"),
                    ("2026-09-14T13:00:00+02:00", "2026-09-14T15:00:00+02:00"),
                ],
                [
                    (s.starts_at.isoformat(), s.ends_at.isoformat())
                    for s in destination.snapshot.mithf_shifts
                ],
            )
            self.assertEqual(
                [
                    (
                        TimeInterval(
                            moment("2026-09-14T08:00:00+02:00"),
                            moment("2026-09-14T10:00:00+02:00"),
                        ),
                    ),
                    (
                        TimeInterval(
                            moment("2026-09-14T13:00:00+02:00"),
                            moment("2026-09-14T14:00:00+02:00"),
                        ),
                    ),
                ],
                [s.sps_intervals for s in destination.snapshot.mithf_shifts],
            )
            self.assertEqual(2, len(destination.snapshot.duos_registrations))
            self.assertTrue(
                all(
                    item.outcome in {Outcome.ALREADY_MATCHED, Outcome.EXCLUDED}
                    for item in final.items
                )
            )

    def test_reapplying_a_split_week_writes_nothing(self):
        destination = MemoryDestinations()
        with SyncState(":memory:") as state:
            self.apply(state, destination, source_shift())
            destination.writes.clear()
            self.apply(state, destination, source_shift())
            self.assertEqual([], destination.writes)
