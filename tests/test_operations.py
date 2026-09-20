from __future__ import annotations

import json
import os
import unittest
from datetime import datetime
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OFFLINE_CONFIG = REPO / "fixtures" / "offline-config.toml"
FIXTURE = REPO / "fixtures" / "representative-week.json"


class OperationsTests(unittest.TestCase):
    def test_resolve_week_defaults_to_monday(self):
        from teamup_shift_sync.operations import resolve_week

        now = datetime.fromisoformat("2026-09-19T12:00:00+02:00")
        week = resolve_week(timezone_name="Europe/Copenhagen", now=now)
        self.assertEqual(week.from_date.isoformat(), "2026-09-14")
        self.assertEqual(week.to_date.isoformat(), "2026-09-20")

    def test_resolve_week_rejects_naive_now(self):
        from teamup_shift_sync.operations import resolve_week

        with self.assertRaises(ValueError):
            resolve_week(
                timezone_name="Europe/Copenhagen",
                now=datetime.fromisoformat("2026-09-19T12:00:00"),
            )

    def test_fixture_preview_matches_cli_digest(self):
        from teamup_shift_sync.operations import fixture_preview_from_paths
        from teamup_shift_sync.report import render_plan
        from teamup_shift_sync.sync import plan_digest

        preview = fixture_preview_from_paths(
            config_path=OFFLINE_CONFIG,
            fixture_path=FIXTURE,
            state_path=Path(":memory:"),
            from_date=None,
            to_date=None,
            now=datetime.fromisoformat("2026-09-20T20:00:00+02:00"),
        )
        self.assertEqual(preview.digest, plan_digest(preview.plan))
        self.assertGreater(len(preview.plan.items), 0)
        # The report still renders; the GUI never parses it.
        self.assertIn("Range:", render_plan(preview.plan))

    def test_connected_preview_validates_identities_as_of_today(self):
        from datetime import date
        from unittest.mock import patch

        from helpers import config, moment

        from teamup_shift_sync import operations
        from teamup_shift_sync.destinations import Destinations
        from teamup_shift_sync.models import DestinationSnapshot

        now = moment("2026-09-19T12:00:00+02:00")
        seen = {}

        def fake_validate(self, when):
            seen["when"] = when

        with (
            patch("teamup_shift_sync.operations.TeamUpClient") as client_cls,
            patch.object(Destinations, "validate", fake_validate),
            patch.object(Destinations, "read", return_value=DestinationSnapshot()),
        ):
            client_cls.return_value.fetch_occurrences.return_value = ()
            operations.preview_connected(
                config=config(),
                transport=object(),
                state_path=Path(":memory:"),
                from_date=date(2026, 8, 31),
                to_date=date(2026, 9, 6),
                now=now,
            )
        # A past week must not invalidate the setup and kick the user back
        # to the current week; identities are confirmed as of today.
        self.assertEqual(seen["when"], now)

    def test_connected_preview_skips_all_day_events(self):
        from datetime import date
        from unittest.mock import patch

        from helpers import config, moment

        from teamup_shift_sync import operations
        from teamup_shift_sync.destinations import Destinations
        from teamup_shift_sync.models import DestinationSnapshot, TeamUpOccurrence
        from teamup_shift_sync.teamup import TeamUpClient

        # A day-off wish carries no hours, so there is nothing to transfer.
        # It must not fail the whole week the way an undecodable shift would.
        # The real shift resolver raises on all-day events, so only the
        # fetch is stubbed here.
        wish = TeamUpOccurrence(
            id="e1",
            series_id="s1",
            title="Ønsker fri",
            starts_at=moment("2026-09-05T00:00:00+02:00"),
            ends_at=moment("2026-09-05T23:59:00+02:00"),
            all_day=True,
            notes="",
            comments=(),
            recurrence_start=None,
            version=None,
            raw={"subcalendar_ids": ["helper"]},
        )
        client = TeamUpClient(config())
        client.fetch_occurrences = lambda *args: (wish,)
        with (
            patch("teamup_shift_sync.operations.TeamUpClient", return_value=client),
            patch.object(Destinations, "validate"),
            patch.object(Destinations, "read", return_value=DestinationSnapshot()),
        ):
            preview = operations.preview_connected(
                config=config(),
                transport=object(),
                state_path=Path(":memory:"),
                from_date=date(2026, 8, 31),
                to_date=date(2026, 9, 6),
                now=moment("2026-09-19T12:00:00+02:00"),
            )
        self.assertEqual(preview.plan.items, ())
        self.assertEqual(
            [day.label for day in preview.week.days][0], "man 31. aug"
        )

    def test_apply_reads_the_same_shifts_as_preview(self):
        from datetime import date
        from unittest.mock import patch

        from helpers import config, moment

        from teamup_shift_sync import operations
        from teamup_shift_sync.destinations import Destinations
        from teamup_shift_sync.models import TeamUpOccurrence
        from teamup_shift_sync.teamup import TeamUpClient

        wish = TeamUpOccurrence(
            id="e1",
            series_id="s1",
            title="Ønsker fri",
            starts_at=moment("2026-09-05T00:00:00+02:00"),
            ends_at=moment("2026-09-05T23:59:00+02:00"),
            all_day=True,
            notes="",
            comments=(),
            recurrence_start=None,
            version=None,
            raw={"subcalendar_ids": ["helper"]},
        )
        applied = {}

        def fake_apply(**kwargs):
            applied.update(kwargs)

        client = TeamUpClient(config())
        client.fetch_occurrences = lambda *args: (wish,)
        with (
            patch("teamup_shift_sync.operations.TeamUpClient", return_value=client),
            patch("teamup_shift_sync.operations.BrowserTransport"),
            patch.object(Destinations, "validate"),
            patch("teamup_shift_sync.operations.apply_plan", fake_apply),
        ):
            operations.apply_live(
                config=config(),
                state_path=Path(":memory:"),
                from_date=date(2026, 8, 31),
                to_date=date(2026, 9, 6),
                expected_digest="digest",
                cdp_url="http://localhost:9222",
                now=moment("2026-09-19T12:00:00+02:00"),
                progress=lambda message: None,
            )
        # Apply must see the same shifts as the approved preview, or its
        # digest check could never match a week containing an all-day event.
        self.assertEqual(applied["shifts"], ())

    def test_home_status_missing_config_is_danish_guidance(self):
        from teamup_shift_sync.operations import home_status

        status = home_status(
            config_path=REPO / "does-not-exist.toml",
            state_path=Path(":memory:"),
            now=datetime.fromisoformat("2026-09-19T12:00:00+02:00"),
        )
        self.assertTrue(status.config_issues)
        self.assertTrue(all(isinstance(issue, str) for issue in status.config_issues))

    def test_home_status_missing_state_means_never_transferred(self):
        from teamup_shift_sync.operations import home_status

        missing = REPO / ".local" / "definitely-not-here-12345.sqlite3"
        if missing.exists():
            missing.unlink()
        status = home_status(
            config_path=OFFLINE_CONFIG,
            state_path=missing,
            now=datetime.fromisoformat("2026-09-19T12:00:00+02:00"),
        )
        self.assertEqual(status.config_issues, ())
        self.assertIsNone(status.last_verified_at)
        self.assertEqual(status.verified_steps, 0)
        self.assertEqual(status.week_start.isoformat(), "2026-09-14")

    def test_app_data_dir_respects_override(self):
        from teamup_shift_sync.operations import app_data_dir

        target = REPO / ".local" / "override-probe"
        os.environ["TEAMUP_SHIFT_SYNC_DATA_DIR"] = str(target)
        try:
            self.assertEqual(app_data_dir(), target)
            self.assertTrue(target.is_dir())
        finally:
            del os.environ["TEAMUP_SHIFT_SYNC_DATA_DIR"]
            target.rmdir()


class WorkerProtocolTests(unittest.TestCase):
    def _request(self, **frame) -> dict:
        from teamup_shift_sync import worker

        raw = worker.process_line(json.dumps(frame))
        self.assertIsNotNone(raw)
        # Stdout carries exactly one JSON frame per line: diagnostics must
        # never leak onto the protocol stream.
        self.assertNotIn("\n", raw.strip())
        return json.loads(raw)

    def test_ping(self):
        frame = self._request(protocol=1, id="p1", method="ping", params={})
        self.assertEqual(frame["event"], "result")
        self.assertEqual(frame["payload"], {"version": 1})

    def test_preview_fixture_returns_typed_payload(self):
        frame = self._request(
            protocol=1,
            id="p2",
            method="preview_fixture",
            params={
                "config_path": str(OFFLINE_CONFIG),
                "fixture_path": str(FIXTURE),
                "state_path": ":memory:",
                "from": "2026-09-14",
                "to": "2026-09-20",
                "now": "2026-09-20T20:00:00+02:00",
            },
        )
        payload = frame["payload"]
        self.assertEqual(frame["event"], "result")
        self.assertIn("digest", payload)
        self.assertIn("counts", payload)
        self.assertEqual(sum(payload["counts"].values()), len(payload["items"]))
        first = payload["items"][0]
        for key in (
            "source_key",
            "system",
            "step_key",
            "outcome",
            "summary",
            "payload",
            "destination_id",
        ):
            self.assertIn(key, first)

    def test_unknown_method_is_danish_error(self):
        frame = self._request(protocol=1, id="p3", method="nope", params={})
        self.assertEqual(frame["event"], "error")
        self.assertEqual(frame["payload"]["code"], "bad_request")
        self.assertTrue(frame["payload"]["message"])
        self.assertIn("detail", frame["payload"])

    def test_wrong_protocol_version_is_bad_protocol(self):
        frame = self._request(protocol=99, id="p4", method="ping", params={})
        self.assertEqual(frame["payload"]["code"], "bad_protocol")

    def test_invalid_json_has_null_id(self):
        from teamup_shift_sync import worker

        frame = json.loads(worker.process_line("not json"))
        self.assertEqual(frame["event"], "error")
        self.assertIsNone(frame["id"])

    def test_missing_fixture_is_fixture_error(self):
        frame = self._request(
            protocol=1,
            id="p5",
            method="preview_fixture",
            params={
                "config_path": str(OFFLINE_CONFIG),
                "fixture_path": str(REPO / "missing.json"),
                "state_path": ":memory:",
            },
        )
        self.assertEqual(frame["payload"]["code"], "fixture_error")


if __name__ == "__main__":
    unittest.main()
