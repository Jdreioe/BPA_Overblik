"""The Danish week view the GUI renders, built from a finished plan."""

from __future__ import annotations

import unittest
from dataclasses import replace

from helpers import config, moment, source_shift

from teamup_shift_sync.models import (
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    Outcome,
    TimeInterval,
)
from teamup_shift_sync.planner import build_plan
from teamup_shift_sync.preview import build_week_preview
from teamup_shift_sync.state import SyncState


class PreviewTests(unittest.TestCase):
    range_start = moment("2026-09-14T00:00:00+02:00")
    range_end = moment("2026-09-21T00:00:00+02:00")
    now = moment("2026-09-20T20:00:00+02:00")

    def week(self, *shifts, destination=None, destination_read=True):
        destination = destination or DestinationSnapshot()
        with SyncState(":memory:") as state:
            plan = build_plan(
                config=config(),
                shifts=shifts,
                destination=destination,
                range_start=self.range_start,
                range_end=self.range_end,
                now=self.now,
                state=state,
                live=True,
            )
        return build_week_preview(
            config=config(),
            shifts=shifts,
            plan=plan,
            destination=destination,
            destination_read=destination_read,
        )

    def blocks(self, week):
        return [(day.label, block) for day in week.days for block in day.blocks]

    def test_two_sps_intervals_are_shown_as_two_parts_of_one_day(self):
        week = self.week(source_shift())
        blocks = self.blocks(week)

        self.assertEqual(["man 14. sep", "man 14. sep"], [day for day, _ in blocks])
        self.assertEqual(
            [
                ("07:30–13:00", "Del 1 af 2", "08:00–10:00"),
                ("13:00–15:00", "Del 2 af 2", "13:00–14:00"),
            ],
            [(b.time_label, b.part_label, b.sps_label) for _, b in blocks],
        )
        self.assertTrue(week.can_apply)
        self.assertEqual(
            "Overfører 2 nye vagter, 2 SPS-tidsrum til MitHF og "
            "2 registreringer til DUOS.",
            week.apply_summary,
        )

    def test_existing_shift_with_only_category_or_helper_writes_can_be_approved(self):
        for change, summary in [
            ("helper", "1 hjælpertildeling"),
            ("sps", "1 SPS-tidsrum"),
            ("meeting", "1 vagtmøde"),
        ]:
            with self.subTest(change=change):
                shift = source_shift(comment="uni 8-10" if change == "sps" else None)
                if change == "meeting":
                    shift = replace(shift, title="P-MØDE")
                destination = DestinationSnapshot(
                    mithf_shifts=(
                        MitHfShift(
                            "mithf-1",
                            shift.starts_at,
                            shift.ends_at,
                            1,
                            None if change == "helper" else "Mit Helper",
                        ),
                    ),
                    duos_registrations=(
                        DuosRegistration(
                            "duos-1",
                            "arrangement",
                            "123",
                            "Almindelig",
                            moment("2026-09-14T08:00:00+02:00"),
                            moment("2026-09-14T10:00:00+02:00"),
                        ),
                    )
                    if change == "sps"
                    else (),
                )
                week = self.week(shift, destination=destination)
                self.assertTrue(week.can_apply)
                self.assertEqual("Ugen er klar til overførsel.", week.headline)
                self.assertIn(summary, week.apply_summary)
                self.assertFalse(
                    any("allerede på plads" in line for line in week.summary)
                )

    def test_duos_update_shows_old_and_new_values(self):
        shift = source_shift(comment="uni 8-10")
        destination = DestinationSnapshot(
            duos_registrations=(
                DuosRegistration(
                    "duos-1",
                    "arrangement",
                    "123",
                    "Almindelig",
                    moment("2026-09-14T08:00:00+02:00"),
                    moment("2026-09-14T09:00:00+02:00"),
                ),
            )
        )
        with SyncState(":memory:") as state:
            state.record_step(
                source_key=shift.key,
                step_key="duos.interval:comment-1:0",
                status="verified",
                destination_id="duos-1",
                source_hash="",
                synced_payload={
                    "arrangement_id": "arrangement",
                    "employee_number": "123",
                    "registration_type": "Almindelig",
                    "starts_at": "2026-09-14T08:00:00+02:00",
                    "ends_at": "2026-09-14T09:00:00+02:00",
                },
                updated_at=self.now,
            )
            plan = build_plan(
                config=config(),
                shifts=(shift,),
                destination=destination,
                range_start=self.range_start,
                range_end=self.range_end,
                now=self.now,
                state=state,
                live=True,
            )
        self.assertTrue(
            any(
                i.system == "duos" and i.outcome == Outcome.WOULD_UPDATE
                for i in plan.items
            )
        )
        week = build_week_preview(
            config=config(),
            shifts=(shift,),
            plan=plan,
            destination=destination,
            destination_read=True,
        )
        self.assertIn(
            "DUOS: registreringen ændres: 08:00–09:00 (1 time) → 08:00–10:00 (2 timer).",
            week.days[0].blocks[0].details,
        )

    def test_blocks_carry_the_helpers_teamup_colour(self):
        week = self.week(source_shift(comment=None))
        ((_, block),) = self.blocks(week)

        # fixture-helper config has no colour; the block renders neutral.
        self.assertEqual("", block.helper_color)

        coloured = replace(config(), teamup_subcalendar_colors={"helper": "#4770d8"})
        with SyncState(":memory:") as state:
            plan = build_plan(
                config=coloured,
                shifts=(source_shift(comment=None),),
                destination=DestinationSnapshot(),
                range_start=self.range_start,
                range_end=self.range_end,
                now=self.now,
                state=state,
                live=True,
            )
        week = build_week_preview(
            config=coloured,
            shifts=(source_shift(comment=None),),
            plan=plan,
            destination=DestinationSnapshot(),
            destination_read=True,
        )
        self.assertEqual("#4770d8", week.days[0].blocks[0].helper_color)

    def test_overnight_shift_appears_in_both_days_with_both_dates(self):
        week = self.week(
            source_shift(
                starts_at="2026-09-15T22:00:00+02:00",
                ends_at="2026-09-16T07:00:00+02:00",
                comment=None,
            )
        )
        blocks = self.blocks(week)

        self.assertEqual(["tir 15. sep", "ons 16. sep"], [day for day, _ in blocks])
        for _, block in blocks:
            self.assertEqual("22:00 15. sep – 07:00 16. sep", block.time_label)
        self.assertEqual(
            [(1320, 1440), (0, 420)],
            [(b.minutes_from, b.minutes_to) for _, b in blocks],
        )
        self.assertEqual(
            [(False, True), (True, False)],
            [(b.continues_before, b.continues_after) for _, b in blocks],
        )

    def test_future_hours_are_explained_rather_than_blocked(self):
        week = self.week(
            source_shift(
                starts_at="2026-09-20T19:00:00+02:00",
                ends_at="2026-09-20T22:00:00+02:00",
                comment="uni 19-21",
            )
        )
        ((_, block),) = self.blocks(week)

        self.assertIn(
            "DUOS: 19:00–21:00 kan overføres, når timerne er afsluttet.",
            block.details,
        )
        self.assertIn(
            "1 SPS-tidsrum kan overføres, når timerne er afsluttet", week.summary
        )

    def test_changed_shift_shows_old_and_new_values(self):
        shift = source_shift(comment="uni 8-10")
        existing = MitHfShift(
            "mithf-1",
            shift.starts_at,
            moment("2026-09-14T14:00:00+02:00"),
            1,
            "Mit Helper",
            (
                TimeInterval(
                    moment("2026-09-14T08:00:00+02:00"),
                    moment("2026-09-14T09:00:00+02:00"),
                ),
            ),
        )
        with SyncState(":memory:") as state:
            state.record_step(
                source_key=shift.key,
                step_key="mithf.create_shift",
                status="verified",
                destination_id="mithf-1",
                source_hash="",
                synced_payload={
                    "starts_at": shift.starts_at.isoformat(),
                    "ends_at": "2026-09-14T14:00:00+02:00",
                    "helper_count": 1,
                },
                updated_at=self.now,
            )
            state.record_step(
                source_key=shift.key,
                step_key="mithf.set_sps",
                status="verified",
                destination_id="mithf-1",
                source_hash="",
                synced_payload={
                    "intervals": [
                        {
                            "starts_at": "2026-09-14T08:00:00+02:00",
                            "ends_at": "2026-09-14T09:00:00+02:00",
                        }
                    ]
                },
                updated_at=self.now,
            )
            destination = DestinationSnapshot(mithf_shifts=(existing,))
            plan = build_plan(
                config=config(),
                shifts=(shift,),
                destination=destination,
                range_start=self.range_start,
                range_end=self.range_end,
                now=self.now,
                state=state,
                live=True,
            )
        week = build_week_preview(
            config=config(),
            shifts=(shift,),
            plan=plan,
            destination=destination,
            destination_read=True,
        )
        ((_, block),) = self.blocks(week)

        self.assertEqual("Ændres", block.status_label)
        self.assertIn(
            "Vagtens tid ændres i MitHF: 07:30–14:00 → 07:30–15:00.", block.details
        )
        self.assertIn("SPS-timer ændres: 08:00–09:00 → 08:00–10:00.", block.details)

    def test_unresolved_instruction_leads_the_week_and_blocks_transfer(self):
        week = self.week(
            source_shift(
                starts_at="2026-09-17T22:00:00+02:00",
                ends_at="2026-09-18T07:00:00+02:00",
                comment="uni 23-1",
            )
        )

        self.assertFalse(week.can_apply)
        self.assertEqual(
            "Løs punkterne under Kræver opmærksomhed først.", week.blocked_reason
        )
        self.assertEqual(1, len(week.attention))
        self.assertEqual(
            "Vagten dækker mere end ét døgn, og uni-linjen siger ikke hvilken dag.",
            week.attention[0].explanation,
        )
        self.assertTrue(
            all(block.status == "attention" for _, block in self.blocks(week))
        )

    def test_empty_and_unchanged_weeks_read_differently(self):
        empty = self.week()
        self.assertEqual("Der er ingen vagter i denne uge.", empty.headline)
        self.assertFalse(empty.can_apply)

        shift = source_shift(comment="uni 8-10")
        matched = self.week(
            shift,
            destination=DestinationSnapshot(
                mithf_shifts=(
                    MitHfShift(
                        "mithf-1",
                        shift.starts_at,
                        shift.ends_at,
                        1,
                        "Mit Helper",
                        (
                            TimeInterval(
                                moment("2026-09-14T08:00:00+02:00"),
                                moment("2026-09-14T10:00:00+02:00"),
                            ),
                        ),
                    ),
                ),
                duos_registrations=(
                    DuosRegistration(
                        "duos-1",
                        "arrangement",
                        "123",
                        "Almindelig",
                        moment("2026-09-14T08:00:00+02:00"),
                        moment("2026-09-14T10:00:00+02:00"),
                    ),
                ),
            ),
        )
        self.assertEqual(
            "Ugen er allerede overført. Der er ingen ændringer.", matched.headline
        )
        self.assertFalse(matched.can_apply)

    def test_unread_destination_never_looks_empty_or_transferable(self):
        week = self.week(source_shift(comment=None), destination_read=False)

        self.assertFalse(week.can_apply)
        self.assertIn("ikke aflæst", week.notice)
        self.assertEqual(
            "Log ind i MitHF og DUOS, så ugen kan aflæses.", week.blocked_reason
        )
        self.assertEqual(1, len(self.blocks(week)))


if __name__ == "__main__":
    unittest.main()
