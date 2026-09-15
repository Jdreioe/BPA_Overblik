from __future__ import annotations

import unittest

from helpers import config, moment, source_shift

from teamup_shift_sync.models import (
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    Outcome,
    TimeInterval,
)
from teamup_shift_sync.planner import build_plan
from teamup_shift_sync.state import SyncState


class PlannerTests(unittest.TestCase):
    range_start = moment("2026-09-14T00:00:00+02:00")
    range_end = moment("2026-09-21T00:00:00+02:00")
    now = moment("2026-09-20T20:00:00+02:00")

    def test_existing_sps_matches_and_differing_sps_requires_review(self):
        shift = source_shift(comment="uni 8-10")
        for end, expected in (
            ("10:00", Outcome.ALREADY_MATCHED),
            ("11:00", Outcome.REVIEW),
        ):
            with self.subTest(end=end):
                existing = MitHfShift(
                    "existing",
                    shift.starts_at,
                    shift.ends_at,
                    1,
                    "Mit Helper",
                    (
                        TimeInterval(
                            moment("2026-09-14T08:00:00+02:00"),
                            moment(f"2026-09-14T{end}:00+02:00"),
                        ),
                    ),
                )
                plan = self.plan(shift, DestinationSnapshot(mithf_shifts=(existing,)))
                sps = next(
                    item for item in plan.items if item.step_key == "mithf.set_sps"
                )
                self.assertEqual(expected, sps.outcome)

    def test_invalid_sps_instruction_blocks_mithf_sps(self):
        plan = self.plan(source_shift(comment="uni 8-10 & later"))
        sps = next(item for item in plan.items if item.step_key == "mithf.set_sps")
        self.assertEqual(Outcome.REVIEW, sps.outcome)

    def plan(self, shift, destination=None, prepare=None):
        destination = destination or DestinationSnapshot()
        with SyncState(":memory:") as state:
            if prepare:
                prepare(state, shift)
            return build_plan(
                config=config(),
                shifts=(shift,),
                destination=destination,
                range_start=self.range_start,
                range_end=self.range_end,
                now=self.now,
                state=state,
            )

    def test_split_sps_creates_independent_duos_steps(self) -> None:
        plan = self.plan(source_shift())
        duos = [item for item in plan.items if item.system == "duos"]
        mithf_sps = next(
            item for item in plan.items if item.step_key == "mithf.set_sps"
        )

        self.assertEqual(2, len(duos))
        self.assertEqual(
            [Outcome.WOULD_CREATE, Outcome.WOULD_CREATE],
            [item.outcome for item in duos],
        )
        self.assertEqual(Outcome.PENDING_MITHF_BUG, mithf_sps.outcome)

    def test_first_duos_match_does_not_block_second_interval(self) -> None:
        existing = DuosRegistration(
            "registration-1",
            "arrangement",
            "123",
            "Almindelig",
            moment("2026-09-14T08:00:00+02:00"),
            moment("2026-09-14T10:00:00+02:00"),
        )
        plan = self.plan(
            source_shift(), DestinationSnapshot(duos_registrations=(existing,))
        )
        duos = [item for item in plan.items if item.system == "duos"]

        self.assertEqual(
            [Outcome.ALREADY_MATCHED, Outcome.WOULD_CREATE],
            [item.outcome for item in duos],
        )

    def test_future_or_ongoing_duos_interval_is_excluded(self) -> None:
        plan = self.plan(
            source_shift(
                starts_at="2026-09-20T19:00:00+02:00",
                ends_at="2026-09-20T22:00:00+02:00",
                comment="uni 19-21",
            )
        )
        duos = next(item for item in plan.items if item.system == "duos")

        self.assertEqual(Outcome.EXCLUDED, duos.outcome)

    def test_resume_assignment_without_recreating_shift(self) -> None:
        shift = source_shift(comment=None)
        existing = MitHfShift(
            "created-before-crash",
            shift.starts_at,
            shift.ends_at,
            1,
            None,
        )
        plan = self.plan(shift, DestinationSnapshot(mithf_shifts=(existing,)))
        create = next(
            item for item in plan.items if item.step_key == "mithf.create_shift"
        )
        assign = next(
            item for item in plan.items if item.step_key == "mithf.assign_helper"
        )

        self.assertEqual(Outcome.ALREADY_MATCHED, create.outcome)
        self.assertEqual(Outcome.WOULD_UPDATE, assign.outcome)
        self.assertEqual("created-before-crash", assign.destination_id)

    def test_source_change_can_update_only_if_destination_was_not_manually_edited(
        self,
    ) -> None:
        changed = source_shift(ends_at="2026-09-14T16:00:00+02:00", comment=None)
        old_payload = {
            "starts_at": changed.starts_at.isoformat(),
            "ends_at": "2026-09-14T15:00:00+02:00",
            "helper_count": 1,
        }
        existing = MitHfShift(
            "known-shift",
            changed.starts_at,
            moment(old_payload["ends_at"]),
            1,
            "Mit Helper",
        )

        def prepare(state, shift):
            state.record_step(
                source_key=shift.key,
                step_key="mithf.create_shift",
                status="created",
                destination_id="known-shift",
                source_hash="old-source",
                synced_payload=old_payload,
                updated_at=self.now,
            )

        plan = self.plan(
            changed, DestinationSnapshot(mithf_shifts=(existing,)), prepare
        )
        create = next(
            item for item in plan.items if item.step_key == "mithf.create_shift"
        )
        self.assertEqual(Outcome.WOULD_UPDATE, create.outcome)

    def test_manual_destination_edit_blocks_source_update(self) -> None:
        changed = source_shift(ends_at="2026-09-14T16:00:00+02:00", comment=None)
        old_payload = {
            "starts_at": changed.starts_at.isoformat(),
            "ends_at": "2026-09-14T15:00:00+02:00",
            "helper_count": 1,
        }
        manually_edited = MitHfShift(
            "known-shift",
            changed.starts_at,
            moment("2026-09-14T14:00:00+02:00"),
            1,
            "Mit Helper",
        )

        def prepare(state, shift):
            state.record_step(
                source_key=shift.key,
                step_key="mithf.create_shift",
                status="created",
                destination_id="known-shift",
                source_hash="old-source",
                synced_payload=old_payload,
                updated_at=self.now,
            )

        plan = self.plan(
            changed, DestinationSnapshot(mithf_shifts=(manually_edited,)), prepare
        )
        create = next(
            item for item in plan.items if item.step_key == "mithf.create_shift"
        )
        self.assertEqual(Outcome.CONFLICTED, create.outcome)

    def test_comment_removal_produces_review_not_replacement(self) -> None:
        shift = source_shift(comment=None)
        old_payload = {
            "arrangement_id": "arrangement",
            "employee_number": "123",
            "registration_type": "Almindelig",
            "starts_at": "2026-09-14T08:00:00+02:00",
            "ends_at": "2026-09-14T10:00:00+02:00",
        }

        def prepare(state, shift):
            state.record_step(
                source_key=shift.key,
                step_key="duos.interval:comment-1:0",
                status="created",
                destination_id="old-registration",
                source_hash="old-comment",
                synced_payload=old_payload,
                updated_at=self.now,
            )

        plan = self.plan(shift, prepare=prepare)
        removed = next(item for item in plan.items if item.system == "duos")
        self.assertEqual(Outcome.REVIEW, removed.outcome)
        self.assertIn("no destructive action", removed.summary)

    def test_partial_parse_failure_blocks_other_duos_writes(self) -> None:
        plan = self.plan(source_shift(comment="uni 8-10 & later"))
        duos = next(item for item in plan.items if item.system == "duos")
        self.assertEqual(Outcome.REVIEW, duos.outcome)


if __name__ == "__main__":
    unittest.main()
