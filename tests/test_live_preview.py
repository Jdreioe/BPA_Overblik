import unittest
from datetime import date
from pathlib import Path
from unittest.mock import patch

from helpers import config, moment
from test_teamup import FakeTransport, detail

from teamup_shift_sync.models import Outcome
from teamup_shift_sync.operations import preview_live
from teamup_shift_sync.report import render_plan
from teamup_shift_sync.teamup import TeamUpClient


class LivePreviewTests(unittest.TestCase):
    def test_reminder_is_excluded_before_helper_resolution(self):
        client = TeamUpClient(config(), FakeTransport([]))
        raw = detail(comments=[])["event"]
        raw["title"] = "Husk at checke vagtplanen på AXP"
        raw["who"] = ""
        occurrence = client._occurrence(raw)
        with (
            patch("teamup_shift_sync.operations.TeamUpClient", return_value=client),
            patch.object(client, "fetch_occurrences", return_value=(occurrence,)),
        ):
            plan = preview_live(
                config=config(),
                state_path=Path(":memory:"),
                from_date=date(2026, 9, 14),
                to_date=date(2026, 9, 20),
                now=moment("2026-09-14T12:00:00+02:00"),
            ).plan
        self.assertEqual([Outcome.EXCLUDED], [item.outcome for item in plan.items])
        self.assertEqual("source.reminder", plan.items[0].step_key)

    def test_live_preview_never_assumes_unread_destinations_are_empty(self):
        client = TeamUpClient(config(), FakeTransport([]))
        raw = detail(comments=[{"id": "1", "message": "uni 8-10 & 13-14"}])["event"]
        occurrence = client._occurrence(raw)
        with (
            patch("teamup_shift_sync.operations.TeamUpClient", return_value=client),
            patch.object(client, "fetch_occurrences", return_value=(occurrence,)),
        ):
            plan = preview_live(
                config=config(),
                state_path=Path(":memory:"),
                from_date=date(2026, 9, 14),
                to_date=date(2026, 9, 20),
                now=moment("2026-09-14T12:00:00+02:00"),
            ).plan
        self.assertNotIn(Outcome.WOULD_CREATE, [item.outcome for item in plan.items])
        duos = [item for item in plan.items if item.system == "duos"]
        self.assertEqual(
            [Outcome.PENDING_INTEGRATION, Outcome.EXCLUDED],
            [item.outcome for item in duos],
        )
        self.assertEqual([2, 1], [item.payload["hours"] for item in duos])
        self.assertNotIn(config().teamup_calendar_key, render_plan(plan))
        self.assertEqual(client.calendar_id, TeamUpClient(config()).calendar_id)
