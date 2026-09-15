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
        if item.step_key == "mithf.create_shift":
            shift = MitHfShift(
                "new", moment(p["starts_at"]), moment(p["ends_at"]), 1, None
            )
            self.snapshot = replace(snapshot, mithf_shifts=(shift,))
            if self.timeout_after_create:
                self.timeout_after_create = False
                raise DestinationError("Response lost")
            return shift.id
        if item.step_key == "mithf.assign_helper":
            # MitHF can return a different shift ID after booking.
            shift = replace(
                snapshot.mithf_shifts[0], id="booked", helper_name=p["helper_name"]
            )
            self.snapshot = replace(snapshot, mithf_shifts=(shift,))
            return shift.id
        if item.step_key == "mithf.set_sps":
            intervals = tuple(
                TimeInterval(moment(i["starts_at"]), moment(i["ends_at"]))
                for i in p["intervals"]
            )
            shift = replace(snapshot.mithf_shifts[0], sps_intervals=intervals)
            self.snapshot = replace(snapshot, mithf_shifts=(shift,))
            return shift.id
        if item.system == "duos":
            entry = DuosRegistration(
                "duos",
                p["arrangement_id"],
                p["employee_number"],
                p["registration_type"],
                moment(p["starts_at"]),
                moment(p["ends_at"]),
            )
            self.snapshot = replace(snapshot, duos_registrations=(entry,))
            return ""
        raise AssertionError("Unexpected write")


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
                "booked", state.get_step(shift.key, "mithf.create_shift").destination_id
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

    def test_unverified_split_sps_blocks_entire_batch(self):
        destination = MemoryDestinations()
        with SyncState(":memory:") as state:
            with self.assertRaisesRegex(DestinationError, "unresolved"):
                self.apply(state, destination, source_shift())
            self.assertEqual([], destination.writes)
